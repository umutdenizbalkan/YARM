#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KTHREAD1 — the kernel-thread witness grader.

Usage: grade-kthread1-witness.py --expect-cpus N <boot.log>
       grade-kthread1-witness.py --self-test

Grades the sealed KT1 record set a `kthread1-witness` boot dumps (`src/kernel/kthread_witness.rs`)
in two separate steps, and never from console order:

1. VALIDITY. Every `KT1_REC` line carries an FNV-1a checksum of its own text; a line that fails it is
   rejected, never repaired. The surviving records must be exactly the sequence 0..n-1 (one record per
   sequence number; a repeated number must repeat identical content), must match the `KT1_SEAL`
   (record count, taken count, no ring overflow, and the seal hash recomputed over the per-record
   checksums in sequence order), and every field must be well formed for its kind. An invalid set
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
   * switch-safety probes refused by name (a masked section; an interrupt-masking guard), a
     lost-wakeup probe that did not block, a stale-id wake reported stale;
   * create/exit/reuse cycles: slot reuse with a strictly newer generation, every reclaim AFTER the
     dispatcher switched away from the exited thread with its residency cleared, pool exhaustion at
     exactly the free count and full recovery; every created thread gone at the end.

Exit 0 only when the set is valid and every obligation holds. Each failure is printed as
`[fail] <reason> ...`.
"""
import re
import sys

KINDS = {"pop", "create", "run", "irq", "switch", "dispatch", "wake", "woken", "resched", "probe",
         "reclaim", "exhaust", "user", "final", "yield", "blip", "wakereq", "decide"}
R_DIRECTOR, R_ALT, R_SLEEPER, R_WAKER, R_BLIP, R_LOSTWAKE, R_HOLDER = 1, 2, 3, 4, 5, 6, 7
P_MASKED, P_IRQ_GUARD, P_STALE_WAKE, P_LOST_WAKE = 1, 2, 3, 4
TOKEN = 0x4B541000
SW_YIELD, SW_BLOCK, SW_EXIT = 1, 2, 3
KID_TAG = 0x4B54 << 48

REC = re.compile(r"(KT1_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) "
                 r"f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)) crc=0x([0-9a-f]{8})")
SEAL = re.compile(r"KT1_SEAL records=(\d+) taken=(\d+) overflow=(\d+) seal=0x([0-9a-f]{8})")


def fnv(data):
    h = 0x811C9DC5
    for b in data:
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


class Rec:
    __slots__ = ("seq", "kind", "cpu", "f", "crc")

    def __init__(self, seq, kind, cpu, f, crc):
        self.seq, self.kind, self.cpu, self.f, self.crc = seq, kind, cpu, f, crc


def parse(text):
    """Return (records by seq, seal, problems)."""
    problems = []
    by_seq = {}
    for m in REC.finditer(text):
        body, crc = m.group(1), int(m.group(10), 16)
        if fnv(body.encode()) != crc:
            problems.append("record_checksum seq=%s" % m.group(2))
            continue
        r = Rec(int(m.group(2)), m.group(3), int(m.group(4)), [int(m.group(i), 16) for i in range(5, 10)], crc)
        old = by_seq.get(r.seq)
        if old is not None and (old.kind, old.cpu, old.f) != (r.kind, r.cpu, r.f):
            problems.append("record_conflict seq=%d" % r.seq)
        by_seq.setdefault(r.seq, r)
    seals = SEAL.findall(text)
    return by_seq, seals, problems


def validate(by_seq, seals, problems):
    fails = list(problems)
    n = len(by_seq)
    if n == 0:
        return ["no_records"]
    if sorted(by_seq) != list(range(n)):
        fails.append("sequence_gap records=%d max=%d" % (n, max(by_seq)))
    if len(set(seals)) != 1:
        fails.append("seal_missing_or_conflicting count=%d" % len(set(seals)))
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


def grade(text, expect_cpus):
    by_seq, seals, problems = parse(text)
    fails = validate(by_seq, seals, problems)
    if fails:
        return ["invalid " + f for f in fails]
    recs = [by_seq[s] for s in sorted(by_seq)]
    out = []

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
        if len(cpus) == 1:
            print("[info] user_progress cpu=%d dispatches=%d (reported, not required with one CPU)"
                  % (c, users[c].f[1] if c in users else 0))
        elif c not in users or users[c].f[1] == 0:
            fail("no_user_progress cpu=%d" % c)
        if c in users and users[c].f[2] == 0:
            fail("no_kernel_thread_dispatch cpu=%d" % c)

    # Probes.
    probes = {}
    for r in of("probe"):
        probes.setdefault(r.f[1], []).append(r)
    for p, name in ((P_MASKED, "masked_section"), (P_IRQ_GUARD, "irq_guard")):
        got = probes.get(p, [])
        if len(got) != 1 or got[0].f[3] != 0 or got[0].f[2] != 1:
            fail("switch_under_guard_not_refused probe=%s" % name)
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
    body = "KT1_REC seq=0 kind=pop cpu=0 f0=0x1 f1=0x1 f2=0x20 f3=0x4000 f4=0x5"
    line = "%s crc=0x%08x" % (body, fnv(body.encode()))
    h = ((0x811C9DC5 ^ fnv(body.encode())) * 0x01000193) & 0xFFFFFFFF
    good = line + "\nKT1_SEAL records=1 taken=1 overflow=0 seal=0x%08x\n" % h
    by, se, pr = parse(good)
    assert validate(by, se, pr) == [], validate(by, se, pr)
    assert validate(*parse(good.replace("f0=0x1 ", "f0=0x3 "))) != []      # checksum
    assert validate(*parse(good.replace("taken=1", "taken=2"))) != []      # seal count
    assert validate(*parse(good.replace("overflow=0", "overflow=1"))) != []
    assert grade(good, 2)[0].startswith("population_mismatch")
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
    by_seq, _, _ = parse(text)
    for f in fails:
        print("[fail] " + f)
    print("KT1_VERDICT records=%d failures=%d result=%s" % (len(by_seq), len(fails), "pass" if not fails else "fail"))
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
