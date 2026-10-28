#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# SMP3-EVIDENCE1 — writes `cases.txt`, the differential corpus, from `base.rec` (a real sealed
# record of a passing two-hart boot). Each case is a list of edits naming base records by their
# seq; `tests/smp3_evidence_differential.rs` applies them and grades the result with both the
# kernel verifier and the Python grader. The records each case targets are FOUND here by what they
# are (the P3 round's shootdown, a merged publication whose group was consumed, ...), never by
# hard-coded number, and every reordering is checked to keep each CPU's own records in program
# order — so every fixture is a record some kernel could have produced. Expected verdicts are
# stated here from the obligations, not taken from either verifier.
#
# Usage: mkcases.py <base.rec> > cases.txt
import re
import sys

STEP = {"S_PRE": 9, "S_OBSERVED": 10, "C_REQ": 27, "C_PRE": 29, "C_OBSERVED": 30, "S_REQ": 7,
        "C_P1_CALL": 22, "S_P1_REPLY": 3}

lines = open(sys.argv[1]).read().split("\n")
roles = re.search(r"s=(\d+):(\d+):(\d+):(\d+) c=(\d+):(\d+):(\d+):(\d+) h1=(\d+):(\d+)", lines[0]).groups()
S = dict(tid=int(roles[0]), asid=int(roles[1]), cpu=int(roles[2]), hart=int(roles[3]))
C = dict(tid=int(roles[4]), asid=int(roles[5]), cpu=int(roles[6]), hart=int(roles[7]))
H1 = int(roles[8])
R = []
for l in lines[1:]:
    m = re.match(r"SMP3_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x(\w+) f1=0x(\w+) f2=0x(\w+) f3=0x(\w+) f4=0x(\w+)", l)
    if m:
        R.append(dict(seq=int(m.group(1)), kind=m.group(2), cpu=int(m.group(3)), f=[int(m.group(i), 16) for i in range(4, 9)]))
out = []


def case(name, expect, *edits):
    out.append("case %s\nexpect %s\n%s\nend\n" % (name, expect, "\n".join(edits)))


def find(start, pred):
    return next((i for i in range(start, len(R)) if pred(R[i])), None)


def user(step, rnd):
    return find(0, lambda r: r["kind"] == "user" and r["f"][2] == STEP[step] and r["f"][3] == rnd)


def op_of(rnd):
    """The P3 round's production operation: its records by kind, inside begin .. end on the requester."""
    req, tgt = (C, S) if rnd % 2 else (S, C)
    q = user(("C_REQ" if rnd % 2 else "S_REQ"), rnd)
    b = find(q, lambda r: r["kind"] == "vm_op_begin" and r["cpu"] == req["cpu"] and r["f"][0] == tgt["asid"])
    e = find(b, lambda r: r["kind"] == "vm_op_end" and r["cpu"] == req["cpu"] and r["f"][2] == R[b]["f"][3])
    op = {}
    for i in range(b, e + 1):
        if R[i]["cpu"] == req["cpu"]:
            op.setdefault(R[i]["kind"], i)
    obs = user(("S_OBSERVED" if rnd % 2 else "C_OBSERVED"), rnd)
    pre = user(("S_PRE" if rnd % 2 else "C_PRE"), rnd)
    res = lambda at: find(at, lambda r: r["kind"] == "residency" and r["f"][0] == tgt["tid"])
    op.update(obs=obs, res_pre=res(pre), res_obs=res(obs), req=req, tgt=tgt)
    for k in ("vm_op_begin", "vm_displaced", "shoot_inputs", "shoot_targets", "fence_request", "fence_done",
              "vm_shootdown", "vm_settled", "vm_op_end"):
        assert k in op, (rnd, k)
    return op


def seq(i):
    return R[i]["seq"]


def none_between(cpu, lo, hi, allowed=()):
    """No record of `cpu` strictly inside (lo, hi] other than `allowed`: moving that CPU's records
    from lo to just after hi keeps its own program order."""
    return not any(R[j]["cpu"] == cpu for j in range(lo + 1, hi + 1) if j not in allowed)


def prev_same_cpu(i):
    return next(j for j in range(i - 1, -1, -1) if R[j]["cpu"] == R[i]["cpu"])


# ── the shootdown decision (a P3 round on each side; both are fenced and credited in the base) ──
o1 = op_of(1)
si, st, fq, fd = (seq(o1[k]) for k in ("shoot_inputs", "shoot_targets", "fence_request", "fence_done"))
tbit = 1 << S["cpu"]
x, t = R[o1["shoot_inputs"]]["f"], R[o1["shoot_targets"]]["f"]
assert t[2] == tbit and x[3] & tbit and x[4] & tbit and t[3 + S["cpu"]] == S["tid"], "round 1 is resident and fenced"
c0 = R[o1["res_pre"]]["f"][1]
cur = "f%d" % (3 + S["cpu"])
entered = "set %d f1=%d" % (seq(o1["res_obs"]), c0 + 2)  # the target entered the kernel in between
gap = ["set %d %s=0" % (st, cur), "set %d f3=0x%x" % (si, x[3] & ~tbit), entered]
other = ["set %d %s=9399" % (st, cur), entered]

case("base: a passing two-hart record (its own merged groups, synchronized and natural gap decisions)", "ok")
case("dispatch gap, residency-only contribution: S's CPU has no current task, its root still loaded", "ok", *gap)
case("another task of the same address space current on the target's CPU", "ok", *other)
case("placement-only contribution", "ok", "set %d f4=0x%x" % (si, x[4] & ~tbit), entered)
case("justified exclusion: S left its CPU and flushed (H1 current, root unloaded), then reactivated",
     "ok", "set %d %s=%d" % (st, cur, H1), "set %d f3=0x%x f4=0x%x" % (si, x[3] & ~tbit, x[4] & ~tbit),
     "set %d f2=0" % st, "del %d" % fq, "del %d" % fd,
     "add 900001 before %d activation %d 0x%x 0x%x 0 0 0" % (seq(o1["obs"]), S["cpu"], S["asid"], (8 << 60) | S["asid"] << 44),
     "set %d f1=%d" % (seq(o1["res_obs"]), c0 + 3))
case("the loaded-root CPU dropped from the target set", "fail shoot_targets_not_the_policy 1", *gap,
     "set %d f2=0" % st)
case("the empty current slot taken to discharge the fence", "fail fence_not_requested_for_the_targets 1", *gap,
     "del %d" % fq, "del %d" % fd)
case("an empty CPU credited with placement", "fail shoot_placement_inconsistent 1", "set %d %s=0" % (st, cur), entered)
case("another task of the address space dropped from the target set", "fail shoot_targets_not_the_policy 1",
     *other, "set %d f2=0" % st, "del %d" % fq, "del %d" % fd)
case("a target named outside the candidates (wake-only)", "fail shoot_inputs_outside_candidates 1",
     "set %d f2=0x%x" % (si, x[2] | (tbit << 32)))
case("a target CPU that is not online", "fail shoot_inputs_outside_candidates 1",
     "set %d f2=0x%x" % (si, x[2] & ~tbit))
case("the decision's inputs missing", "fail shoot_inputs_missing 1", "del %d" % si)
case("the decision's inputs not recorded", "fail shoot_inputs_unrecorded 1", "set %d f4=0xffffffffffffffff" % si)
case("the decision recorded by a CPU that is not the requester", "fail shoot_targets_missing 1",
     "set %d cpu=%d" % (si, S["cpu"]), "set %d cpu=%d" % (st, S["cpu"]))
case("the fence requested for other CPUs than the decision named", "fail fence_targets_not_the_decision 1",
     "set %d f4=0x%x" % (fq, 1 << C["cpu"]))
case("the fence names the requester's own hart", "fail requester_fenced_itself 1",
     "set %d f2=0x%x" % (fq, 1 << C["hart"]), "set %d f2=0x%x" % (fd, 1 << C["hart"]))
case("the fence names a hart no target runs on", "fail fence_harts_not_the_targets 1",
     "set %d f2=0x20" % fq, "set %d f2=0x20" % fd)
case("the fence completion failed", "fail fence_failed 1", "set %d f4=0xfffffffffffffffd" % fd)
# A removed (or another CPU's) completion leaves a LATER request's completion of the same mapping as
# the first one after this request: a substituted completion, refused by its generation.
case("the fence completion missing (a later request's completion found in its place)", "fail fence_stale_completion 1",
     "del %d" % fd)
case("the fence completion duplicated", "fail fence_duplicate_completion 1", "dup 900002 after %d of %d" % (fd, fd))
case("the fence completion names another request's generation", "fail fence_stale_completion 1",
     "set %d f3=0x%x" % (fd, R[o1["fence_done"]]["f"][3] + 1))
case("the fence completion substituted by another CPU's", "fail fence_stale_completion 1",
     "set %d cpu=%d" % (fd, S["cpu"]))
case("settlement before the fence completed", "fail settlement_outside_operation 1",
     "move %d before %d" % (seq(o1["vm_settled"]), fd))
case("an excluded target never reactivated before its observation", "fail off_cpu_target_not_reactivated 1",
     "set %d %s=%d" % (st, cur, H1), "set %d f3=0x%x f4=0x%x" % (si, x[3] & ~tbit, x[4] & ~tbit),
     "set %d f2=0" % st, "del %d" % fq, "del %d" % fd, "set %d f1=%d" % (seq(o1["res_obs"]), c0 + 3))
# The mirror direction (round 2: C on CPU 0 is the target, S on CPU 1 requests).
o2 = op_of(2)
x2, t2 = R[o2["shoot_inputs"]]["f"], R[o2["shoot_targets"]]["f"]
cbit = 1 << C["cpu"]
c0b = R[o2["res_pre"]]["f"][1]
case("dispatch gap in the other direction (C's CPU empty, its root loaded)", "ok",
     "set %d f%d=0" % (seq(o2["shoot_targets"]), 3 + C["cpu"]),
     "set %d f3=0x%x" % (seq(o2["shoot_inputs"]), x2[3] & ~cbit), "set %d f1=%d" % (seq(o2["res_obs"]), c0b + 2))
case("the other direction's fence completion missing", "fail fence_stale_completion 2", "del %d" % seq(o2["fence_done"]))
# With every later completion of the mapping gone too, nothing completes the request at all.
case("no completion of the mapping after the request", "fail fence_completion_missing 1",
     *["del %d" % r["seq"] for r in R[o1["fence_done"]:] if r["kind"] == "fence_done" and r["cpu"] == C["cpu"]
       and r["f"][:3] == R[o1["fence_done"]]["f"][:3]])

# ── hart identity: the same record relabelled as a boot whose boot hart is hart 1 ──
relabel = "harts %d %d" % (C["hart"], S["hart"])
case("harts relabelled: CPU ids and hart ids differ", "ok", relabel)
case("relabelled, the fence names the target's CPU index as its hart", "fail requester_fenced_itself 1", relabel,
     "set %d f2=0x%x" % (fq, tbit), "set %d f2=0x%x" % (fd, tbit))
p1 = find(0, lambda r: r["kind"] == "ipi_published" and r["f"][2] == 0)
case("relabelled, a publication names its target's CPU index as the hart", "fail ipi_publication_wrong_hart", relabel,
     "set %d f1=%d" % (seq(p1), R[p1]["f"][0]))

# ── IPI attribution from the mailbox protocol ──
cons = dict(((r["f"][0], r["cpu"], r["f"][1]), i) for i, r in enumerate(R) if r["kind"] == "ipi_consumed")


def consumption(i):
    r = R[i]
    return cons.get((r["cpu"], r["f"][0], r["f"][4]))


def next_request(i):
    return find(i + 1, lambda r: r["kind"] == "ipi_requested" and r["cpu"] == R[i]["cpu"])


def late_move(merged):
    """A publication recorded before the consumption of its group, movable (with its request) to just
    after that consumption without reordering its sender's records."""
    for i, r in enumerate(R):
        if r["kind"] != "ipi_published" or r["f"][2] != 0 or r["f"][3] != merged:
            continue
        c, q = consumption(i), next_request(i)
        if c is None or c < i or q is None or q > c:
            continue
        if none_between(r["cpu"], i, c, allowed=(q,)):
            return i, q, c
    return None


m = late_move(1)
assert m, "a movable merged publication"
case("a merged publication recorded after the arrival that consumed its group", "ok",
     "move %d after %d" % (seq(m[0]), seq(m[2])), "move %d after %d" % (seq(m[1]), seq(m[0])))
f = late_move(0)
assert f, "a movable fresh publication"
case("a fresh publication recorded after the arrival that consumed it", "ok",
     "move %d after %d" % (seq(f[0]), seq(f[2])), "move %d after %d" % (seq(f[1]), seq(f[0])))


def overlap():
    """Group g consumed before group g+1 is published, the consumer's records movable past g+1's."""
    for i, r in enumerate(R):
        if r["kind"] != "ipi_consumed":
            continue
        src, dst, k = r["f"][0], r["cpu"], r["f"][1]
        a = prev_same_cpu(i)
        if R[a]["kind"] != "ipi_arrived" or R[a]["f"][0] != 1 << src:
            continue
        nxt = find(i + 1, lambda x: x["kind"] == "ipi_published" and x["cpu"] == src and x["f"][0] == dst
                   and x["f"][3] == 0 and x["f"][4] == k + 1)
        q = None if nxt is None else next_request(nxt)
        if q is None:
            continue
        if none_between(dst, a, q, allowed=(i,)):
            return a, i, q
    return None


ov = overlap()
assert ov, "an overlap candidate"
case("a fresh publication recorded between the swap that consumed the previous group and its arrival record",
     "ok", "move %d after %d" % (seq(ov[1]), seq(ov[2])), "move %d after %d" % (seq(ov[0]), seq(ov[2])))
ci = next(i for i, r in enumerate(R) if r["kind"] == "ipi_consumed" and r["f"][1] >= 2
          and R[prev_same_cpu(i)]["f"][0] == 1 << r["f"][0])
case("an arrival's consumption record missing", "fail ipi_consumption_unbound", "del %d" % seq(ci))
case("a consumption recorded after another record of its CPU", "fail ipi_consumption_unbound",
     "move %d after %d" % (seq(ci), seq(find(ci + 1, lambda r: r["cpu"] == R[ci]["cpu"]))))
case("a consumption attributed to a group already taken", "fail ipi_duplicate_attribution",
     "set %d f1=%d" % (seq(ci), R[ci]["f"][1] - 1))
case("a consumption naming a source its arrival did not consume", "fail ipi_consumption_unbound",
     "set %d f0=%d" % (seq(ci), 1 - R[ci]["f"][0]))
src, dst = R[ci]["f"][0], R[ci]["cpu"]
last_k = max(k for (s_, d_, k) in cons if (s_, d_) == (src, dst))
case("an arrival consuming a group its source never opened", "fail ipi_unexplained_arrival",
     "add 900003 after %d ipi_arrived %d 0x%x 0x1 0 0 0" % (len(R) - 1, dst, 1 << src),
     "add 900004 after 900003 ipi_consumed %d %d %d 0 0 0" % (dst, src, last_k + 1))
lk = cons[(src, dst, last_k)]
la = prev_same_cpu(lk)
assert R[la]["kind"] == "ipi_arrived" and R[la]["f"][0] == 1 << src
case("a lost publication: its group's arrival and consumption missing", "fail ipi_undelivered",
     "del %d" % seq(la), "del %d" % seq(lk))
fr = next(i for i, r in enumerate(R) if r["kind"] == "ipi_published" and r["f"][3] == 0 and r["f"][4] >= 2)
case("a fresh publication repeating its sender's previous group (stale)", "fail ipi_group_inconsistent",
     "set %d f4=%d" % (seq(fr), R[fr]["f"][4] - 1))
mg = next(i for i, r in enumerate(R) if r["kind"] == "ipi_published" and r["f"][3] == 1)
case("a merged publication outside its sender's current group", "fail ipi_group_inconsistent",
     "set %d f4=%d" % (seq(mg), R[mg]["f"][4] + 1))
case("a publication naming another hart than its target's", "fail ipi_publication_wrong_hart",
     "set %d f1=%d" % (seq(p1), 7))
rq = find(0, lambda r: r["kind"] == "ipi_requested")
case("a firmware request refused", "fail ipi_firmware_refused", "set %d f3=0xfffffffffffffffe" % seq(rq))
case("more arrivals on a CPU than successful requests to it", "fail ipi_arrivals_exceed_requests 0",
     *["del %d" % r["seq"] for r in R if r["kind"] == "ipi_requested" and r["f"][0] == 0])
sys.stdout.write("# SMP3-EVIDENCE1 differential corpus — generated by mkcases.py from base.rec; do not edit.\n")
sys.stdout.write("\n".join(out))
