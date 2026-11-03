#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KTHREAD1 — the kernel-thread witness grader.

Usage: grade-kthread1-witness.py --expect-cpus N <boot.log>
       grade-kthread1-witness.py --self-test

Grades the sealed KT1 record set a `kthread1-witness` boot dumps (`src/kernel/kthread_witness.rs`)
in two separate steps, and never from console order:

1. VALIDITY. The witness dumps the record set twice, each pass closed by a `KT1_SEAL`: the console
   is shared with raw byte writers that take no line lock, so a line can arrive damaged. Every
   `KT1_REC` and `KT1_SEAL` line carries an FNV-1a checksum of its own text; a line that fails it
   (or no longer parses) is damaged: it counts for nothing and is never repaired, and the number of
   damaged lines is reported. The intact records must be exactly the sequence 0..n-1 (a record whose
   every copy is damaged is missing; two intact copies of a number must be identical), the intact
   seals must agree, and the records must match that seal (record count, taken count, no ring
   overflow, and the seal hash recomputed over the per-record checksums in sequence order), and
   every field must be well formed for its kind. An invalid set
   is graded no further: nothing is awarded from records that cannot be trusted.
2. COVERAGE. The population is DECLARED by the run (`pop`, the dispatching CPUs at the start) and must
   equal the CPUs the caller expects. Every obligation below is then required on every CPU of that
   population, bound by kernel-thread identity (`kid`, the incarnation id) and sequence number:

   * creation: one sleeper and two alternators homed on the CPU, each with a stack of the declared
     size;
   * execution: every alternator round runs on its home CPU, on its own stack, with its private stack
     contents intact, every patterned register intact across an interrupt of ITS OWN that is recorded
     between its previous scheduling event and this round, and its callee-saved registers intact
     across the round's voluntary yield (the holder's round whose scheduling point switched away
     has no separate yield: that switch was its turn); after the hold, skipping the two rounds
     that straddle the woken sleeper, the two alternators strictly alternate;
   * kernel-origin interrupts: timer interrupts taken by a kernel thread of the CPU, on that thread's
     stack, at a non-zero kernel PC;
   * remote wake: a wake of the CPU's sleeper issued from ANOTHER CPU (outcome: enqueued), then a
     reschedule interrupt taken by a kernel thread of the target CPU, then that CPU's holder honouring
     the pending reschedule at its scheduling point, then the sleeper resuming with the right token;
   * user progress: user tasks marked Running on the CPU during the kernel-thread phase (with one
     CPU reported, not required: the only user workload needs a secondary);
   and once per run:
   * switch-safety probes refused by name (a masked section; an interrupt-masking guard; a real
     plain SpinLock across yield, block, nesting, a partial release and a timer interrupt, with
     scheduling restored after the final release), a lost-wakeup probe that did not block, a
     stale-id wake reported stale;
   * create/exit/reuse cycles: slot reuse with a strictly newer generation, every reclaim AFTER the
     dispatcher switched away from the exited thread with its residency cleared, pool exhaustion at
     exactly the free count and full recovery; every created thread gone at the end.

Exit 0 only when the set is valid and every obligation holds. Each failure is printed as
`[fail] <reason> ...`.
"""
import re
import sys

KINDS = {"pop", "create", "run", "irq", "switch", "dispatch", "wake", "woken", "resched", "probe",
         "reclaim", "exhaust", "user", "final", "yield", "blip", "wakereq", "decide", "director",
         "uwork", "uarm", "utimeout"}
R_DIRECTOR, R_ALT, R_SLEEPER, R_WAKER, R_BLIP, R_LOSTWAKE, R_HOLDER, R_PLAINHOLD = 1, 2, 3, 4, 5, 6, 7, 8
P_MASKED, P_IRQ_GUARD, P_STALE_WAKE, P_LOST_WAKE, P_PLAIN, P_PLAIN_OTHER = 1, 2, 3, 4, 5, 6
# KSwitchRefusal codes as the producer records them (kthread_witness.rs `refusal_code`).
R_PLAIN_LOCK_HELD = 7
TOKEN = 0x4B541000
SW_YIELD, SW_BLOCK, SW_EXIT = 1, 2, 3
KID_TAG = 0x4B54 << 48

# KTHREAD1-ACCEPTANCE §4 — the contract the run must declare, anchored here (src/kernel/kthread.rs
# KTHREAD_CAPACITY / KTHREAD_STACK_BYTES, src/kernel/kthread_witness.rs ROUNDS / CYCLES). Required
# populations derive from these and from the create records, never from what a run reports alone.
CAPACITY, STACK_BYTES, ROUNDS_REQ, CYCLES = 32, 16384, 5, 6
# P, the one-CPU user workload (src/kernel/apsched_witness.rs P_TID; its S1 rounds in
# src/arch/<isa>/apsched_witness.S): round k stores k * S1_MUL ^ S1_SALT and reports what it reads
# back; a receive with a real deadline must end TimedOut (9, the receive ABI's error code).
P_TID, S1_MUL, S1_SALT, TIMED_OUT = 9600, 0x9E3779B97F4A7C15, 0x5151000000005151, 9
USER_KINDS = {"uwork", "uarm", "utimeout"}
# Records a thread writes about itself: each must fall inside one of its own running intervals.
IN_THREAD = {"run", "yield", "decide", "resched", "blip", "woken", "irq"}
# Records only the director writes, on its own CPU.
DIRECTOR_ONLY = {"exhaust", "final", "user"}


def kid_ok(kid):
    return kid & (0xFFFF << 48) == KID_TAG and (kid & 0xFFFF) < CAPACITY and (kid >> 16) & 0xFFFFFFFF != 0


def kslot(kid):
    return kid & 0xFFFF


def kgen(kid):
    return (kid >> 16) & 0xFFFFFFFF


def replay(recs):
    """VALIDITY, part 2: replay every lifecycle event in sequence order.

    What each event is evidence of (see the producers in src/kernel/kthread.rs and
    src/kernel/kthread_witness.rs):
      create   the incarnation exists, with this role, home and stack; written inside
               `KThreadPool::create` BEFORE the incarnation is enqueued, so nothing about it can
               precede this record;
      dispatch it entered execution on its home CPU (residency set, state Running); written
               immediately before the switch into it;
      switch   it left the CPU and the dispatcher cleared residency (f2 must be 0): reason 1 yield,
               2 block, 3 exit;
      reclaim  its slot returned to the free list, after an exit switch; written before the slot is
               published, so no reuse can precede it;
      in-thread records (run, yield, decide, resched, blip, woken, irq, the lost-wake probe) are
               written by the thread, or by an interrupt it took, while it runs;
      final    the director's observation at the end. A final saying "gone" is an observation, not
               the reclaim itself: it may precede the reclaim record (the slot is Free before the
               record is written), but never the exit switch.
    The director is the one thread with no create record: it declares itself (`director`, record
    1) and is exempt from the interval checks by that identity alone. Returns (impossible,
    incomplete): events a correct producer cannot emit, and obligations the set does not discharge.
    """
    bad, incomplete = [], []
    pops = [r for r in recs if r.kind == "pop"]
    dirs = [r for r in recs if r.kind == "director"]
    if len(pops) != 1 or pops[0].seq != 0:
        return ["population_not_declared_first"], []
    if len(dirs) != 1 or dirs[0].seq != 1:
        return ["director_not_declared"], []
    pop, d = pops[0], dirs[0]
    if (pop.f[2], pop.f[3], pop.f[4]) != (CAPACITY, STACK_BYTES, ROUNDS_REQ):
        bad.append("contract_mismatch capacity=%d stack=%d rounds=%d" % (pop.f[2], pop.f[3], pop.f[4]))
    cpus = {c for c in range(8) if pop.f[0] & (1 << c)}
    dkid, dhome, dlow, dhigh = d.f[0], d.f[1], d.f[2], d.f[3]
    if not kid_ok(dkid) or d.cpu != dhome or dhigh - dlow != STACK_BYTES:
        bad.append("director_identity kid=0x%x" % dkid)
    base = dlow - kslot(dkid) * STACK_BYTES
    th = {}
    holder_of = {kslot(dkid): dkid}
    waker = None
    exhaust = None
    # KTHREAD-CLOSURE1 §A: one execution owner per CPU, from the producer's execution stamp
    # (`res` = the incarnation resident on the CPU, `ord` = its dispatch ordinal). A dispatch opens
    # a new ordinal; until the CPU's next ordinal, every record of that CPU belongs to that
    # interval. The director's dispatches are not recorded (it polls by yielding), so an interval
    # whose ordinal has no dispatch record is the director's, and only the director may be resident
    # in it. Nothing is exempted by role: the director's own records must carry it as resident in
    # one of its own intervals.
    owner = {}           # cpu -> kid of the open RECORDED interval (None: closed)
    cur_ord = {}         # cpu -> ordinal the CPU's records currently belong to
    recorded_ival = {}   # cpu -> True when the current ordinal opened with a dispatch record
    RES_NONE_KINDS = {"switch", "reclaim"} | USER_KINDS
    for r in recs:
        k, kid = r.kind, r.f[0]
        c = r.cpu
        if c in cur_ord and r.ord < cur_ord[c]:
            bad.append("ord_backwards seq=%d cpu=%d ord=%d after=%d" % (r.seq, c, r.ord, cur_ord[c]))
        elif c not in cur_ord or r.ord != cur_ord[c]:
            # The CPU dispatched since its last record: whatever interval was open has ended.
            if owner.get(c) is not None:
                bad.append("interval_ended_without_switch seq=%d cpu=%d kid=0x%x" % (r.seq, c, owner[c]))
                owner[c] = None
            cur_ord[c] = r.ord
            recorded_ival[c] = k == "dispatch"
        elif k == "dispatch":
            bad.append("dispatch_without_new_interval seq=%d cpu=%d kid=0x%x" % (r.seq, c, kid))
        # Who the producer saw resident when it took this record.
        if k == "dispatch":
            if r.res != kid:
                bad.append("dispatch_resident_mismatch seq=%d kid=0x%x res=0x%x" % (r.seq, kid, r.res))
        elif r.res == 0:
            if k not in RES_NONE_KINDS:
                bad.append("record_without_resident seq=%d kind=%s" % (r.seq, k))
        elif r.res == dkid:
            if c != dhome or recorded_ival.get(c):
                bad.append("director_resident_outside_its_interval seq=%d cpu=%d" % (r.seq, c))
        elif owner.get(c) != r.res:
            bad.append("resident_not_the_cpu_owner seq=%d cpu=%d res=0x%x owner=%s"
                       % (r.seq, c, r.res, "0x%x" % owner[c] if owner.get(c) else "none"))
        if k in ("pop", "director"):
            continue
        if k == "create":
            role, home, low, high = r.f[1], r.f[2], r.f[3], r.f[4]
            if r.cpu != dhome or r.res != dkid:
                bad.append("create_not_by_director seq=%d" % r.seq)
            if not kid_ok(kid):
                bad.append("create_identity seq=%d kid=0x%x" % (r.seq, kid))
                continue
            if kid in th:
                bad.append("create_duplicate kid=0x%x" % kid)
                continue
            if home not in cpus:
                bad.append("create_home_outside_population kid=0x%x home=%d" % (kid, home))
            if high - low != STACK_BYTES or low - kslot(kid) * STACK_BYTES != base:
                bad.append("create_stack_not_the_slots kid=0x%x" % kid)
            prev = holder_of.get(kslot(kid))
            if prev == dkid:
                bad.append("create_in_director_slot kid=0x%x" % kid)
            elif prev is not None:
                if th[prev]["state"] != "reclaimed":
                    bad.append("slot_reused_before_reclaim slot=%d kid=0x%x" % (kslot(kid), kid))
                elif kgen(kid) <= kgen(prev):
                    bad.append("reuse_generation_not_newer slot=%d" % kslot(kid))
            holder_of[kslot(kid)] = kid
            th[kid] = dict(role=role, home=home, state="created", finals=[], runs=0)
            if role == R_WAKER:
                waker = kid
            continue
        if k in ("dispatch", "switch", "reclaim"):
            t = th.get(kid)
            if t is None:
                bad.append("%s_unknown_thread seq=%d kid=0x%x" % (k, r.seq, kid))
                continue
            if r.cpu != t["home"]:
                bad.append("%s_off_home seq=%d kid=0x%x cpu=%d" % (k, r.seq, kid, r.cpu))
            role_field = {"dispatch": r.f[1], "switch": r.f[3], "reclaim": r.f[4]}[k]
            if role_field != t["role"]:
                bad.append("role_changed seq=%d kid=0x%x" % (r.seq, kid))
            if k == "dispatch":
                if t["state"] not in ("created", "off"):
                    bad.append("dispatch_while_%s seq=%d kid=0x%x" % (t["state"], r.seq, kid))
                    continue
                t["state"], t["runs"] = "running", t["runs"] + 1
                owner[r.cpu] = kid
            elif k == "switch":
                if owner.get(r.cpu) != kid:
                    bad.append("switch_by_non_owner seq=%d cpu=%d kid=0x%x owner=%s"
                               % (r.seq, r.cpu, kid, "0x%x" % owner[r.cpu] if owner.get(r.cpu) else "none"))
                else:
                    owner[r.cpu] = None
                if t["state"] != "running":
                    bad.append("switch_while_%s seq=%d kid=0x%x" % (t["state"], r.seq, kid))
                    continue
                if r.f[2] != 0:
                    bad.append("switched_out_while_resident seq=%d kid=0x%x" % (r.seq, kid))
                if r.f[1] not in (SW_YIELD, SW_BLOCK, SW_EXIT):
                    bad.append("switch_reason seq=%d" % r.seq)
                t["state"] = "exited" if r.f[1] == SW_EXIT else "off"
            else:
                if owner.get(r.cpu) is not None or r.res != 0:
                    bad.append("reclaimed_while_cpu_owned seq=%d cpu=%d" % (r.seq, r.cpu))
                if t["state"] != "exited":
                    bad.append("reclaimed_before_switch_away seq=%d kid=0x%x state=%s" % (r.seq, kid, t["state"]))
                    continue
                if (r.f[1], r.f[2]) != (kslot(kid), kgen(kid)):
                    bad.append("reclaim_identity kid=0x%x" % kid)
                t["state"] = "reclaimed"
            continue
        if k in IN_THREAD or (k == "probe" and r.f[1] == P_LOST_WAKE):
            t = th.get(kid)
            if t is None:
                bad.append("%s_unknown_thread seq=%d kid=0x%x" % (k, r.seq, kid))
            elif t["state"] != "running" or r.cpu != t["home"] or r.res != kid:
                bad.append("%s_outside_running_interval seq=%d kid=0x%x state=%s cpu=%d res=0x%x"
                           % (k, r.seq, kid, t["state"], r.cpu, r.res))
            elif k == "blip" and (r.f[1], r.f[2]) != (kslot(kid), kgen(kid)):
                bad.append("blip_identity kid=0x%x" % kid)
            continue
        if k == "probe":
            if r.cpu != dhome or r.res != dkid:
                bad.append("probe_not_by_director seq=%d" % r.seq)
            if r.f[1] in (P_MASKED, P_IRQ_GUARD, P_PLAIN, P_PLAIN_OTHER):
                if kid != dkid:
                    bad.append("probe_not_by_director seq=%d" % r.seq)
            elif r.f[1] == P_STALE_WAKE:
                if kid not in th or th[kid]["state"] != "reclaimed":
                    bad.append("stale_probe_names_a_live_or_unknown_thread kid=0x%x" % kid)
            else:
                bad.append("probe_kind seq=%d" % r.seq)
            continue
        if k in ("wakereq", "wake"):
            by_waker = (waker is not None and r.res == waker and r.cpu == th[waker]["home"]
                        and th[waker]["state"] == "running")
            if r.res != dkid and not by_waker:
                bad.append("%s_issuer_not_running seq=%d cpu=%d" % (k, r.seq, r.cpu))
            if kid not in th or th[kid]["role"] != R_SLEEPER:
                bad.append("%s_subject_not_a_sleeper seq=%d" % (k, r.seq))
            continue
        if k in USER_KINDS:
            # A user-origin entry the trap path authenticated as P's, on a CPU of the population,
            # with no kernel thread resident and no recorded kernel-thread interval open there.
            if kid != P_TID or r.cpu not in cpus:
                bad.append("user_record_identity seq=%d tid=%d cpu=%d" % (r.seq, kid, r.cpu))
            if r.res != 0 or owner.get(r.cpu) is not None:
                bad.append("user_execution_while_kernel_thread_owns_cpu seq=%d cpu=%d" % (r.seq, r.cpu))
            continue
        if k in DIRECTOR_ONLY:
            if r.cpu != dhome or r.res != dkid:
                bad.append("%s_not_by_director seq=%d" % (k, r.seq))
            if k == "final":
                t = th.get(kid)
                if t is None:
                    bad.append("final_unknown_thread kid=0x%x" % kid)
                    continue
                if r.f[1] == 0 and t["state"] not in ("exited", "reclaimed"):
                    bad.append("final_gone_while_%s kid=0x%x" % (t["state"], kid))
                if r.f[2] != t["role"]:
                    bad.append("role_changed seq=%d kid=0x%x" % (r.seq, kid))
                t["finals"].append(r)
            elif k == "exhaust":
                exhaust = r
            continue
        bad.append("unexpected_record seq=%d kind=%s" % (r.seq, k))
    for kid, t in th.items():
        if len(t["finals"]) > 1:
            bad.append("final_duplicate kid=0x%x" % kid)
    # Completeness: every created thread ran, exited, was reclaimed, and has its one final state.
    for kid, t in th.items():
        if len(t["finals"]) == 0:
            incomplete.append("final_missing kid=0x%x role=%d" % (kid, t["role"]))
        elif t["finals"][0].f[1] != 0:
            incomplete.append("thread_left_behind kid=0x%x state=%d role=%d"
                              % (kid, t["finals"][0].f[1], t["role"]))
        elif t["state"] != "reclaimed":
            incomplete.append("not_reclaimed kid=0x%x state=%s" % (kid, t["state"]))
        if t["runs"] == 0 and len(t["finals"]) == 1 and t["finals"][0].f[1] == 0:
            incomplete.append("never_dispatched kid=0x%x" % kid)
    # Populations, from the create records and the anchored contract.
    def count(role, home=None):
        return sum(1 for t in th.values() if t["role"] == role and (home is None or t["home"] == home))
    # (The per-CPU alternator/sleeper population is graded from the same create records below,
    # by `creation_population`.)
    if count(R_WAKER) != (1 if len(cpus) > 1 else 0):
        incomplete.append("waker_population %d" % count(R_WAKER))
    if count(R_LOSTWAKE) != 1:
        incomplete.append("lost_wake_population %d" % count(R_LOSTWAKE))
    if count(R_BLIP) != CYCLES + 1:
        incomplete.append("cycle_population %d" % count(R_BLIP))
    if exhaust is None or count(R_HOLDER) != exhaust.f[0]:
        incomplete.append("holder_population created=%d declared=%s"
                          % (count(R_HOLDER), exhaust.f[0] if exhaust else "missing"))
    if count(R_PLAINHOLD) != (1 if len(cpus) > 1 else 0):
        incomplete.append("plain_holder_population %d" % count(R_PLAINHOLD))
    known = {R_ALT, R_SLEEPER, R_WAKER, R_BLIP, R_LOSTWAKE, R_HOLDER, R_PLAINHOLD}
    for kid, t in th.items():
        if t["role"] not in known:
            bad.append("unknown_role kid=0x%x role=%d" % (kid, t["role"]))
    return bad, incomplete

# KTHREAD-CLOSURE1 §A: `res`/`ord` are the CPU's execution stamp, taken by the producer from the
# dispatcher's own state in the same masked section as the sequence number: the incarnation resident
# on the CPU (0 = none) and the CPU's kernel-thread dispatch ordinal.
REC = re.compile(r"(KT1_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) "
                 r"f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+) res=0x([0-9a-f]+) ord=0x([0-9a-f]+))"
                 r" crc=0x([0-9a-f]{8})")
SEAL = re.compile(r"(KT1_SEAL records=(\d+) taken=(\d+) overflow=(\d+) seal=0x([0-9a-f]{8})) crc=0x([0-9a-f]{8})")


def fnv(data):
    h = 0x811C9DC5
    for b in data:
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


class Rec:
    __slots__ = ("seq", "kind", "cpu", "f", "res", "ord", "crc")

    def __init__(self, seq, kind, cpu, f, crc, res=0, ord_=0):
        self.seq, self.kind, self.cpu, self.f, self.crc = seq, kind, cpu, f, crc
        self.res, self.ord = res, ord_


def parse(text):
    """Return (records by seq, intact seals, problems, damaged line count)."""
    problems = []
    by_seq = {}
    intact = 0
    for m in REC.finditer(text):
        body, crc = m.group(1), int(m.group(12), 16)
        if fnv(body.encode()) != crc:
            continue
        intact += 1
        r = Rec(int(m.group(2)), m.group(3), int(m.group(4)), [int(m.group(i), 16) for i in range(5, 10)], crc,
                int(m.group(10), 16), int(m.group(11), 16))
        old = by_seq.get(r.seq)
        if old is not None and (old.kind, old.cpu, old.f, old.res, old.ord) != (r.kind, r.cpu, r.f, r.res, r.ord):
            problems.append("record_conflict seq=%d" % r.seq)
        by_seq.setdefault(r.seq, r)
    seals = []
    for m in SEAL.finditer(text):
        if fnv(m.group(1).encode()) == int(m.group(6), 16):
            seals.append(m.groups()[1:5])
    damaged = text.count("KT1_REC") + text.count("KT1_SEAL") - intact - len(seals)
    return by_seq, seals, problems, damaged


def validate(by_seq, seals, problems, damaged=0):
    fails = list(problems)
    n = len(by_seq)
    if n == 0:
        return ["no_records"]
    if sorted(by_seq) != list(range(n)):
        fails.append("sequence_gap records=%d max=%d" % (n, max(by_seq)))
    if not seals:
        fails.append("seal_missing")
    elif len(set(seals)) != 1:
        fails.append("seal_conflicting count=%d" % len(set(seals)))
    else:
        records, taken, overflow, seal = int(seals[0][0]), int(seals[0][1]), int(seals[0][2]), int(seals[0][3], 16)
        if records != n or taken != n:
            fails.append("seal_count records=%d taken=%d parsed=%d" % (records, taken, n))
        if overflow != 0:
            fails.append("ring_overflow lost=%d" % overflow)
        h = 0x811C9DC5
        for s in sorted(by_seq):
            h = ((h ^ by_seq[s].crc) * 0x01000193) & 0xFFFFFFFF
        if h != seal:
            fails.append("seal_hash expected=0x%08x got=0x%08x" % (h, seal))
    for r in by_seq.values():
        if r.kind not in KINDS:
            fails.append("unknown_kind seq=%d kind=%s" % (r.seq, r.kind))
        if r.cpu >= 8:
            fails.append("cpu_out_of_range seq=%d cpu=%d" % (r.seq, r.cpu))
    return fails


def single_cpu_coexistence(recs):
    """P's checked work interleaved with kernel-thread execution, and a real deadline delivered
    while the kernel threads keep running. Every record here is a user-origin entry the trap path
    authenticated as P's (the replay checked its identity)."""
    out = []
    work = [r for r in recs if r.kind == "uwork"]
    kt = [r.seq for r in recs if r.kind in ("dispatch", "run")]
    good = [r for r in work if r.f[2] == (r.f[1] * S1_MUL ^ S1_SALT) & 0xFFFFFFFFFFFFFFFF]
    if len(good) != len(work):
        out.append("user_work_value_wrong rounds=%s" % [r.f[1] for r in work if r not in good][:4])
    rounds = [r.f[1] for r in work]
    if not rounds:
        out.append("user_work_missing")
    elif rounds != list(range(rounds[0], rounds[0] + len(rounds))):
        out.append("user_rounds_not_consecutive")
    # Interleaving: kernel-thread execution, then P's work, then kernel-thread execution again —
    # at least twice over.
    turns, after_kt = 0, False
    for r in recs:
        if r.kind in ("dispatch", "run"):
            after_kt = True
        elif r.kind == "uwork" and after_kt:
            # P's work after kernel-thread execution, with kernel-thread execution after it.
            if any(s > r.seq for s in kt):
                turns += 1
            after_kt = False
    if turns < 2:
        out.append("user_and_kernel_threads_not_interleaved turns=%d" % turns)
    # KTHREAD-CLOSURE1 §A — every round is validated before any coverage is awarded. P's rounds
    # run work(k) -> arm(k) -> timeout(k) -> work(k+1) on one CPU, one system call at a time, so
    # the only legal user-record sequence is exactly that, each step once, rounds consecutive. The
    # census boundaries may cut the two outer rounds: recording starts when the director is
    # created, asynchronously to P, so the FIRST recorded round may begin at its arm or its result
    # (it earns no coverage: its arm is unrecorded); the director stops P's phase, then recording,
    # so the LAST round may end after its work or its arm. That is valid unfinished work. Anything
    # else — a duplicate arm or result, a step for another round, a result before its arm, a round
    # without its result before the next round, another address space — is an impossible
    # completed operation and fails, whatever other rounds earn. (Task and CPU identity, and the
    # CPU's ownership, are the replay's: it refuses a user record from another task, outside the
    # population, or inside a kernel-thread interval before any of this runs.)
    users = [r for r in recs if r.kind in USER_KINDS]
    expect, rnd, asid = "uwork", None, None
    arms, outs, invalid = {}, [], []
    for r in users:
        if r.kind == "uwork":
            if asid is None:
                asid = r.f[3]
            elif r.f[3] != asid:
                invalid.append("user_round_address_space_changed seq=%d" % r.seq)
        if rnd is None and r.kind != "uwork":
            # The leading round, cut by the start of recording.
            rnd, expect = r.f[1], r.kind
        want = rnd if r.kind != "uwork" else (r.f[1] if rnd is None else rnd + 1)
        if r.kind != expect or r.f[1] != want:
            invalid.append("user_sequence_invalid seq=%d got=%s round=%d expected=%s round=%s"
                           % (r.seq, r.kind, r.f[1], expect, want))
            break
        if r.kind == "uwork":
            rnd, expect = r.f[1], "uarm"
        elif r.kind == "uarm":
            arms[rnd], expect = r, "utimeout"
        else:
            a = arms.get(rnd)
            if r.f[2] != TIMED_OUT:
                invalid.append("user_deadline_result round=%d result=%d" % (r.f[1], r.f[2]))
            if a is None:
                pass  # the leading round's arm precedes recording
            elif r.f[3] - a.f[3] < a.f[2]:
                invalid.append("user_deadline_not_elapsed round=%d armed=%d at=%d deadline=%d"
                               % (r.f[1], a.f[3], r.f[3], a.f[2]))
            if a is not None and r.f[4] < a.f[4]:
                invalid.append("user_idle_count_backwards round=%d" % r.f[1])
            outs.append(r)
            expect = "uwork"
    out.extend(invalid)
    delivered = 0
    for t in outs:
        a = arms.get(t.f[1])
        if a is None or a.seq > t.seq or t.f[2] != TIMED_OUT:
            continue
        between = sum(1 for s in kt if a.seq < s < t.seq)
        later = any(s > t.seq for s in kt)
        no_idle = a.f[4] == t.f[4]
        elapsed = t.f[3] - a.f[3] >= a.f[2]
        nxt = any(w.f[1] == t.f[1] + 1 and w.seq > t.seq for w in work)
        if between >= 2 and later and no_idle and elapsed and nxt:
            delivered += 1
    if not outs:
        out.append("user_deadline_never_delivered")
    elif delivered == 0:
        out.append("user_deadline_not_delivered_while_kernel_threads_ran")
    return out


def grade(text, expect_cpus):
    by_seq, seals, problems, _ = parse(text)
    fails = validate(by_seq, seals, problems)
    if fails:
        return ["invalid " + f for f in fails]
    recs = [by_seq[s] for s in sorted(by_seq)]
    impossible, incomplete = replay(recs)
    if impossible:
        return ["invalid lifecycle_" + f for f in impossible]
    out = list(incomplete)

    def fail(msg):
        out.append(msg)

    def of(kind):
        return [r for r in recs if r.kind == kind]

    pops = of("pop")
    if len(pops) != 1 or pops[0].seq != 0:
        return ["population_not_declared_first"]
    pop = pops[0].f[0]
    want = (1 << min(expect_cpus, 8)) - 1
    if pop != want:
        fail("population_mismatch declared=0x%x expected=0x%x" % (pop, want))
    stack_bytes = pops[0].f[3]
    rounds = pops[0].f[4]
    cpus = [c for c in range(8) if pop & (1 << c)]

    # Kernel threads, by incarnation id.
    kt = {}
    for r in of("create"):
        kid, role, home, low, high = r.f
        if kid & (0xFFFF << 48) != KID_TAG:
            fail("create_not_a_kernel_thread seq=%d" % r.seq)
        if high - low != stack_bytes or low == 0:
            fail("create_stack_size kid=0x%x" % kid)
        if kid in kt:
            fail("create_duplicate_id kid=0x%x" % kid)
        kt[kid] = dict(role=role, home=home, low=low, high=high, seq=r.seq)

    def on_stack(kid, addr):
        k = kt.get(kid)
        return k is not None and k["low"] <= addr < k["high"]

    alts = {c: [k for k, v in kt.items() if v["role"] == R_ALT and v["home"] == c] for c in cpus}
    sleepers = {c: [k for k, v in kt.items() if v["role"] == R_SLEEPER and v["home"] == c] for c in cpus}
    irqs = of("irq")
    for c in cpus:
        if len(alts[c]) != 2 or len(sleepers[c]) != 1:
            fail("creation_population cpu=%d alternators=%d sleepers=%d" % (c, len(alts[c]), len(sleepers[c])))

    # Every interrupt record: an authenticated kernel thread of that CPU, on its own stack.
    for r in irqs:
        kid, kind, pc, frame, role = r.f
        k = kt.get(kid)
        if k is None or k["home"] != r.cpu:
            fail("irq_not_on_a_kernel_thread_of_its_cpu seq=%d" % r.seq)
        elif not on_stack(kid, frame):
            fail("irq_frame_off_stack seq=%d kid=0x%x" % (r.seq, kid))
        if pc == 0 or kind not in (1, 2):
            fail("irq_origin_unrecorded seq=%d" % r.seq)

    # Execution: rounds, stacks, registers, the interrupt each round waited for.
    events = {}  # kid -> seqs of its scheduling events (dispatch) and rounds
    for r in of("dispatch"):
        events.setdefault(r.f[0], []).append(r.seq)
    for c in cpus:
        for kid in alts[c]:
            runs = [r for r in of("run") if r.f[0] == kid]
            ylds = [r for r in of("yield") if r.f[0] == kid]
            got = [r.f[1] for r in runs]
            if len(got) < rounds or got != list(range(1, len(got) + 1)):
                fail("rounds_incomplete kid=0x%x got=%s" % (kid, got))
            # Every round ends in an explicit yield, except the holder's round whose scheduling point
            # switched away: that switch was the round's turn.
            given = {r.f[4] for r in of("decide") if r.f[0] == kid
                     if any(x.f[0] == kid and x.seq > r.seq and x.f[1] == 1 for x in of("resched"))}
            if [r.f[1] for r in ylds] != [g for g in got if g not in given]:
                fail("yields_incomplete kid=0x%x" % kid)
            prev = kt[kid]["seq"]
            for r in runs:
                if r.cpu != c:
                    fail("run_off_home kid=0x%x round=%d cpu=%d" % (kid, r.f[1], r.cpu))
                if not on_stack(kid, r.f[2]):
                    fail("run_off_stack kid=0x%x round=%d" % (kid, r.f[1]))
                if not r.f[4] & 1:
                    fail("private_stack_corrupted kid=0x%x round=%d" % (kid, r.f[1]))
                if not r.f[4] & 2:
                    fail("registers_clobbered_by_interrupt kid=0x%x round=%d" % (kid, r.f[1]))
                if not r.f[4] & 8:
                    fail("no_interrupt_during_round kid=0x%x round=%d" % (kid, r.f[1]))
                if not any(i.f[0] == kid and prev < i.seq < r.seq for i in irqs):
                    fail("round_interrupt_unrecorded kid=0x%x round=%d" % (kid, r.f[1]))
                prev = r.seq
            for r in ylds:
                if r.f[2] != 1 or r.f[3] != 0:
                    fail("callee_saved_clobbered_by_switch kid=0x%x round=%d" % (kid, r.f[1]))
        # Alternation, measured after this CPU's hold: the holder's honoured scheduling point is its
        # turn, so the two alternators strictly alternate through it; the sleeper runs in between.
        # The first two rounds after the decision are skipped (they straddle the woken sleeper);
        # from then on the two must strictly alternate, for at least four rounds.
        held = [r for r in of("decide") if r.cpu == c and r.f[0] in alts[c]]
        if len(cpus) == 1:
            # One CPU: no hold and no decision; strict alternation over every round.
            late = [r.f[0] for r in of("run") if r.f[0] in alts[c]]
            if held:
                fail("hold_on_single_cpu cpu=%d" % c)
            if len(late) < 4:
                fail("alternation_too_short cpu=%d rounds=%d" % (c, len(late)))
            if any(a == b for a, b in zip(late, late[1:])):
                fail("alternation_broken cpu=%d order=%s" % (c, ["%x" % (k & 0xFFFF) for k in late]))
        elif len(held) != 1:
            fail("hold_missing cpu=%d" % c)
        else:
            late = [r.f[0] for r in of("run") if r.f[0] in alts[c] and r.seq > held[0].seq][2:]
            if len(late) < 4:
                fail("alternation_too_short cpu=%d rounds=%d" % (c, len(late)))
            if any(a == b for a, b in zip(late, late[1:])):
                fail("alternation_broken cpu=%d order=%s" % (c, ["%x" % (k & 0xFFFF) for k in late]))
        if not any(r.cpu == c and r.f[1] == 1 and r.f[0] in kt for r in irqs):
            fail("no_timer_interrupt_on_kernel_thread cpu=%d" % c)

    # Remote wake chain per CPU: request (recorded before the wake) -> reschedule interrupt on a
    # kernel thread of the target CPU, naming the requester where the hardware names one -> the
    # holder's decision input (recorded before its scheduling point) with the reschedule pending ->
    # the sleeper resuming with the right token; plus the wake's own outcome (enqueued) and the
    # holder's honoured point.
    if len(cpus) == 1 and sleepers[cpus[0]]:
        # One CPU: the director wakes the sleeper locally, after it blocked; it must resume with the
        # right token, after the wake.
        c = cpus[0]
        s_ = sleepers[c][0]
        reqs = [r for r in of("wakereq") if r.f[0] == s_]
        outs = [r for r in of("wake") if r.f[0] == s_ and r.f[4] == 1]
        woke = [r for r in of("woken") if r.f[0] == s_]
        if len(reqs) != 1 or len(outs) != 1 or reqs[0].cpu != c or reqs[0].f[1] != c:
            fail("local_wake_missing cpu=%d" % c)
        elif outs[0].f[2] != 1:
            fail("local_wake_not_enqueued cpu=%d outcome=%d" % (c, outs[0].f[2]))
        elif len(woke) != 1 or woke[0].f[1] != TOKEN + c or woke[0].seq < reqs[0].seq:
            fail("woken_thread_did_not_resume_after_wake cpu=%d" % c)
    for c in cpus:
        if len(cpus) < 2 or not sleepers[c]:
            continue
        s_ = sleepers[c][0]
        reqs = [r for r in of("wakereq") if r.f[0] == s_]
        outs = [r for r in of("wake") if r.f[0] == s_ and r.f[4] == 1]
        if len(reqs) != 1 or len(outs) != 1:
            fail("remote_wake_missing cpu=%d" % c)
            continue
        q, w = reqs[0], outs[0]
        if q.cpu == c or q.f[1] != c or w.cpu != q.cpu or w.f[2] != 1:
            fail("remote_wake_not_remote_or_not_enqueued cpu=%d waker_cpu=%d outcome=%d" % (c, q.cpu, w.f[2]))
        arrival = [r for r in irqs if r.cpu == c and r.f[1] == 2 and r.seq > q.seq
                   and r.f[0] in kt and kt[r.f[0]]["home"] == c
                   and ((r.f[4] >> 8) == 0 or (r.f[4] >> 8) & (1 << q.cpu))]
        if not arrival:
            fail("remote_wake_without_reschedule_arrival cpu=%d" % c)
            continue
        dec = [r for r in of("decide") if r.cpu == c and r.f[0] in alts[c] and r.seq > arrival[0].seq]
        if not dec or dec[0].f[1] != 1:
            fail("reschedule_not_pending_at_decision cpu=%d" % c)
            continue
        done = [r for r in of("resched") if r.cpu == c and r.f[0] == dec[0].f[0] and r.seq > dec[0].seq]
        if not done or done[0].f[1] != 1:
            fail("reschedule_not_honoured cpu=%d" % c)
        woke = [r for r in of("woken") if r.f[0] == s_]
        if len(woke) != 1 or woke[0].cpu != c or woke[0].f[1] != TOKEN + c or woke[0].seq < dec[0].seq:
            fail("woken_thread_did_not_resume_after_decision cpu=%d" % c)

    # User progress.
    # With one CPU the only user workload (the AP-scheduling witness) does not start, so no user
    # task need be runnable during the kernel-thread phase: there, user progress is reported only.
    users = {r.f[0]: r for r in of("user")}
    for c in cpus:
        if len(cpus) > 1 and (c not in users or users[c].f[1] == 0):
            fail("no_user_progress cpu=%d" % c)
        if c in users and users[c].f[2] == 0:
            fail("no_kernel_thread_dispatch cpu=%d" % c)

    # KTHREAD1-ACCEPTANCE §5 — one CPU: P and the kernel threads in the same measured phase.
    if len(cpus) == 1:
        for f_ in single_cpu_coexistence(recs):
            fail(f_)

    # Probes.
    probes = {}
    for r in of("probe"):
        probes.setdefault(r.f[1], []).append(r)
    for p, name in ((P_MASKED, "masked_section"), (P_IRQ_GUARD, "irq_guard")):
        got = probes.get(p, [])
        if len(got) != 1 or got[0].f[3] != 0 or got[0].f[2] != 1:
            fail("switch_under_guard_not_refused probe=%s" % name)
    # KTHREAD-CLOSURE1 §B: a real plain SpinLock held across yield, block, a nested acquisition, a
    # partial release and a timer interrupt taken on the holder is refused every time, by name;
    # the count follows the guards exactly; the final release restores scheduling.
    plain = probes.get(P_PLAIN, [])
    if len(plain) != 1:
        fail("switch_under_guard_not_refused probe=plain_lock records=%d" % len(plain))
    else:
        f2, f3, f4 = plain[0].f[2], plain[0].f[3], plain[0].f[4]
        codes = [(f2 >> (8 * i)) & 0xFF for i in range(4)]
        counts = [(f4 >> (8 * i)) & 0xFF for i in range(4)]
        if codes != [R_PLAIN_LOCK_HELD] * 4 or f3 & 0xF:
            fail("switch_under_guard_not_refused probe=plain_lock codes=%s switched=0x%x" % (codes, f3 & 0xF))
        if not (f3 >> 4) & 1:
            fail("plain_lock_release_did_not_restore_scheduling")
        if counts != [1, 2, 1, 0]:
            fail("plain_lock_count_wrong counts=%s" % counts)
        if not (f4 >> 32) & 1:
            fail("plain_lock_probe_without_interrupt")
    # KTHREAD-CLOSURE1 §B: with two CPUs or more, another thread's live plain guard counts against
    # that thread only (its own yield refused), and the director stays eligible.
    other = probes.get(P_PLAIN_OTHER, [])
    if expect_cpus > 1:
        if len(other) != 1:
            fail("plain_lock_attribution_probe records=%d" % len(other))
        else:
            f2, f3, f4 = other[0].f[2], other[0].f[3], other[0].f[4]
            mine, theirs, their_code = f2 & 0xFF, (f2 >> 8) & 0xFF, (f2 >> 16) & 0xFF
            if (mine, theirs, their_code, f3, f4) != (0, 1, R_PLAIN_LOCK_HELD, 0, 1):
                fail("plain_lock_misattributed director=%d holder=%d holder_refusal=%d director_refusal=%d held_seen=%d"
                     % (mine, theirs, their_code, f3, f4))
    elif other:
        fail("plain_lock_attribution_probe records=%d with one CPU" % len(other))
    lost = probes.get(P_LOST_WAKE, [])
    if len(lost) != 1 or lost[0].f[2] != 0:
        fail("lost_wakeup")
    stale = probes.get(P_STALE_WAKE, [])
    if len(stale) != 1 or stale[0].f[2] != 3:
        fail("stale_wake_accepted")
    elif stale[0].f[4] != 4 or (stale[0].f[3] & 0xFFFF) != (stale[0].f[0] & 0xFFFF):
        # The probe only proves something if the replacement in the same slot was parked: a wake
        # that skipped the incarnation check would then have enqueued it.
        fail("stale_probe_target_not_parked_in_same_slot")

    # Lifecycle: reuse with newer generations; reclaim strictly after the switch away.
    switches = of("switch")
    for r in of("reclaim"):
        kid, slot, gen, free, role = r.f
        out_ = [s for s in switches if s.f[0] == kid and s.f[1] == SW_EXIT and s.seq < r.seq and s.cpu == r.cpu]
        if not out_ and role not in (R_DIRECTOR, R_HOLDER, 0):
            fail("reclaimed_before_switch_away kid=0x%x" % kid)
        elif out_ and out_[-1].f[2] != 0:
            fail("reclaimed_while_resident kid=0x%x" % kid)
        if (kid & 0xFFFF) != slot or ((kid >> 16) & 0xFFFFFFFF) != gen:
            fail("reclaim_identity kid=0x%x" % kid)
    blips = of("blip")
    if len(blips) < 6:
        fail("cycles_incomplete blips=%d" % len(blips))
    reclaimed = {r.f[0]: r.seq for r in of("reclaim")}
    last = {}
    for b in blips:
        kid, slot, gen = b.f[0], b.f[1], b.f[2]
        if slot in last:
            pk, pgen = last[slot]
            if gen <= pgen:
                fail("reuse_generation_not_newer slot=%d" % slot)
            if pk not in reclaimed or reclaimed[pk] > kt.get(kid, {}).get("seq", -1):
                fail("slot_reused_before_reclaim slot=%d" % slot)
        last[slot] = (kid, gen)
        if kid not in reclaimed:
            fail("cycle_not_reclaimed kid=0x%x" % kid)
    if not any(len([b for b in blips if b.f[1] == s]) > 1 for s in set(b.f[1] for b in blips)):
        fail("no_slot_reuse")
    ex = of("exhaust")
    if len(ex) != 1 or ex[0].f[2] != 1 or ex[0].f[0] != ex[0].f[1] or ex[0].f[3] != ex[0].f[1]:
        fail("exhaustion_or_recovery %s" % ([hex(x) for x in ex[0].f] if ex else "missing"))
    for r in of("final"):
        if r.f[1] != 0:
            fail("thread_left_behind kid=0x%x state=%d role=%d" % (r.f[0], r.f[1], r.f[2]))
    if not of("final"):
        fail("final_states_missing")
    return out


def self_test():
    body = "KT1_REC seq=0 kind=pop cpu=0 f0=0x1 f1=0x1 f2=0x20 f3=0x4000 f4=0x5 res=0x0 ord=0x0"
    line = "%s crc=0x%08x" % (body, fnv(body.encode()))
    h = ((0x811C9DC5 ^ fnv(body.encode())) * 0x01000193) & 0xFFFFFFFF
    seal = "KT1_SEAL records=1 taken=1 overflow=0 seal=0x%08x" % h
    good = line + "\n" + "%s crc=0x%08x\n" % (seal, fnv(seal.encode()))

    def resealed(text, old, new):
        s2 = seal.replace(old, new)
        return text.replace(seal, s2).replace("crc=0x%08x\n" % fnv(seal.encode()), "crc=0x%08x\n" % fnv(s2.encode()))

    assert validate(*parse(good)) == [], validate(*parse(good))
    assert parse(good)[3] == 0
    assert validate(*parse(good.replace("f0=0x1 ", "f0=0x3 "))) != []      # checksum
    assert parse(good.replace("f0=0x1 ", "f0=0x3 "))[3] == 1                # counted damaged
    assert validate(*parse(good.replace("taken=1", "taken=2"))) != []      # seal checksum
    assert validate(*parse(resealed(good, "taken=1", "taken=2"))) != []    # seal count
    assert validate(*parse(resealed(good, "overflow=0", "overflow=1"))) != []
    damaged = line.replace("kind=", "3kind=")                              # a stray console byte
    assert validate(*parse(damaged + "\n" + good)) == []                   # the other copy is intact
    assert validate(*parse(damaged + "\n" + good.replace(line, damaged))) != []
    # A one-record set is valid transport but declares no director: the replay refuses it.
    assert grade(good, 2) == ["invalid lifecycle_director_not_declared"], grade(good, 2)
    print("KT1_GRADER_SELF_TEST result=pass")


def main(argv):
    if argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 4 or argv[1] != "--expect-cpus":
        print(__doc__)
        return 2
    text = open(argv[3], "rb").read().decode("utf-8", "replace")
    fails = grade(text, int(argv[2]))
    by_seq, _, _, damaged = parse(text)
    if damaged:
        print("[info] damaged_lines=%d (a damaged line counts for nothing; each record needs one intact copy)" % damaged)
    for f in fails:
        print("[fail] " + f)
    print("KT1_VERDICT records=%d failures=%d result=%s" % (len(by_seq), len(fails), "pass" if not fails else "fail"))
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
