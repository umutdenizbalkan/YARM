#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3 — the independent grader for `scripts/qemu-riscv64-smp3-witness-smoke.sh`.
#
# Usage: grade-riscv64-smp3-witness.py <boot.log> <sequence-check exit status> <pin> <identity>
#        grade-riscv64-smp3-witness.py --records <file>   (records only: SMP3_ROLES + SMP3_REC lines)
#        grade-riscv64-smp3-witness.py --steps            (the step table, for the differential test)
#
# Re-derives every graded edge from the raw sealed record (`SMP3_REC`), written independently of
# the kernel's verifier (`kernel::boot::smp3_record::verify`), and then also requires the kernel's
# counts and verdict to agree. Never retries; exits non-zero on any failure.
#
# SMP3-EVIDENCE1 — `verify_records` is the record-level verification, obligation for obligation in
# the kernel verifier's order and with its failure codes, so the two can be run on the same cases
# and must agree on acceptance and on the first violated obligation
# (`tests/smp3_evidence_differential.rs`). Two repairs: a shootdown's correctness is checked against
# the production policy over the inputs its owner recorded it consumed (placement ∪ loaded-root
# residency, without the requester) — never "resident" inferred from `current` — and IPI wakes are
# attributed through the mailbox protocol (publication groups and consumption ordinals), never
# through record order.
import re
import sys

STEP_FAIL = 100
STEPS = [
    ("S_ENTERED", 1), ("S_P1_RESUMED", 2), ("S_P1_REPLY", 3), ("S_WIN_A_OK", 4), ("S_P2B_CALL", 5),
    ("S_P2B_REPLY_OK", 6), ("S_REQ", 7), ("S_REQ_DONE", 8), ("S_PRE", 9), ("S_OBSERVED", 10),
    ("S_MUT_NR3", 11), ("S_MUT_OK", 12), ("S_DONE", 13), ("S_P2B_SENT", 14),
    ("C_ENTERED", 21), ("C_P1_CALL", 22), ("C_P1_RESUMED", 23), ("C_P2A_CALL", 24),
    ("C_P2A_REPLY_OK", 25), ("C_WIN_B_OK", 26), ("C_REQ", 27), ("C_REQ_DONE", 28), ("C_PRE", 29),
    ("C_OBSERVED", 30), ("C_MUT_NR3", 31), ("C_MUT_OK", 32), ("C_DONE", 33), ("C_P2A_SENT", 34),
    ("H_ENTERED", 41), ("H_SERVED", 42),
]
CODE = dict(STEPS)
NO_CODE = (1 << 64) - 1
PUB_WAKE, PUB_RELEASE, PUB_KICK = 0, 1, 2
ORIGIN_USER, ORIGIN_IDLE = 0, 1
WINDOW_NONE, WINDOW_S_A, WINDOW_C_B = 0, 1, 2
PHASE_P2, PHASE_P3 = 2, 3
P1_ROUNDS, P1_MIN_IPI_TO_C = 8, 2
P2_ATTEMPTS, P2_MIN_CREDITED_PER_DIRECTION = 6, 1
TLB_ROUNDS, TLB_MIN_CREDITED_PER_DIRECTION = 24, 2
MUT_ROUNDS = 4
SSTATUS_FS_VS = (3 << 13) | (3 << 9)
U64_MAX = (1 << 64) - 1
NCPU = 64

# The kernel's counts, by the names its dump prints them under.
COUNT_KEYS = [
    "records", "published", "fresh", "requests", "arrivals", "consumed", "empty", "merged", "user_fp_vs_off",
    "p1_parked", "p1_ipi_to_s", "p1_ipi_to_c", "p1_timer_first", "p1_busy", "p1_preceded", "p1_merged",
    "p2_attempts", "p2_credited_s", "p2_credited_c", "p2_uncredited", "p2_in_window", "p2_outside",
    "p2_displaced", "tlb_rounds", "credited_s", "credited_c", "interfered", "off_cpu", "not_ready",
    "not_current", "mutual_rounds", "overlapped", "contended", "mutual_off_cpu", "mutual_not_current",
    "settled_after_completion", "settled_local_only",
]


class Refusal(Exception):
    def __init__(self, code, at=0):
        Exception.__init__(self, code)
        self.code, self.at = code, at


def shl(bit):
    """`1u64.checked_shl(bit as u32)`: None past 63."""
    bit &= 0xFFFFFFFF
    return (1 << bit) if bit < 64 else None


def verify_records(recs, roles):
    """The record-level verification. Returns (failures, counts, attempts): every failure as
    (code, at, detail) in the kernel verifier's order (its verdict is the first), the counts, and
    every graded attempt as (phase, n, target_cpu) -> (eligible, reason, outcome)."""
    S, C, W = roles["s"], roles["c"], roles["w"]
    known = [S, C, roles["h1"], roles["h0"]]
    n = len(recs)
    fails, attempts = [], {}
    cnt = dict((k, 0) for k in COUNT_KEYS)
    cnt["records"] = n

    def fail(code, at, detail=""):
        fails.append((code, at, detail))

    def need(x, code):
        if x is None:
            raise Refusal(code)
        return x

    def find(start, pred):
        for i in range(max(start, 0), n):
            if pred(recs[i]):
                return i
        return None

    def user(start, role, step, rnd=None):
        c = CODE.get(step, NO_CODE)
        return find(start, lambda r: r["kind"] == "user" and r["f"][0] == role["tid"] and r["f"][2] == c
                    and (rnd is None or r["f"][3] == rnd))

    def residency(role, step, rnd):
        c = CODE.get(step, NO_CODE)
        i = find(0, lambda r: r["kind"] == "residency" and r["f"][0] == role["tid"] and r["f"][2] == c and r["f"][3] == rnd)
        return None if i is None else recs[i]["f"][1]

    def arr_origin(r): return r["f"][1] & 0xFF
    def arr_window(r): return (r["f"][1] >> 8) & 0xFF
    def arr_entries(r): return r["f"][1] >> 16
    def arr_tid(r): return r["f"][3] & 0xFFFFFFFF
    def arr_asid(r): return r["f"][3] >> 32
    def ready_met(r): return (r["f"][0] >> 16) & 1 == 1
    def helper_parked(r): return (r["f"][0] >> 17) & 1 == 1

    def ready(start, waker_cpu, target_cpu, phase, rnd):
        return find(start, lambda r: r["kind"] == "ready" and r["cpu"] == waker_cpu and r["f"][0] & 0xFF == target_cpu
                    and (r["f"][0] >> 8) & 0xFF == phase and r["f"][3] == rnd)

    def hart_mask_of(cpus):
        harts = 0
        for cpu in range(64):
            if not cpus & (1 << cpu):
                continue
            if cpu == S["cpu"]:
                hart = S["hart"]
            elif cpu == C["cpu"]:
                hart = C["hart"]
            else:
                return None
            b = shl(hart)
            if b is None:
                return None
            harts |= b
        return harts

    # ── IPI POPULATION: groups and consumption ordinals (the mailbox protocol) ──
    def population():
        c = dict(published=0, fresh=0, merged=0, requests=0, arrivals=0, empty=0, consumed=0)
        opened, took, owed, requests, taken, cons = {}, {}, {}, {}, {}, []
        for r in recs:
            cpu, k, f = r["cpu"] & 63, r["kind"], r["f"]
            if owed.get(cpu, (0, 0))[0] and k != "ipi_consumed":
                raise Refusal("ipi_consumption_unbound", owed[cpu][1])
            if k == "ipi_published" and f[2] in (PUB_WAKE, PUB_KICK):
                c["published"] += 1
                key = (cpu, f[0] & 63)
                g = opened.get(key, 0)
                if f[3] == 0:
                    if f[4] != g + 1:
                        raise Refusal("ipi_group_inconsistent", r["seq"])
                    opened[key] = f[4]
                    c["fresh"] += 1
                else:
                    if f[3] != 1 or f[4] == 0 or f[4] != g:
                        raise Refusal("ipi_group_inconsistent", r["seq"])
                    c["merged"] += 1
            elif k == "ipi_requested":
                if f[3] != 0:
                    raise Refusal("ipi_firmware_refused", r["seq"])
                requests[f[0] & 63] = requests.get(f[0] & 63, 0) + 1
                c["requests"] += 1
            elif k == "park_released":
                taken[cpu] = taken.get(cpu, 0) + 1
            elif k == "ipi_arrived":
                taken[cpu] = taken.get(cpu, 0) + 1
                c["arrivals"] += 1
                if f[0] == 0:
                    c["empty"] += 1
                owed[cpu] = (f[0], r["seq"])
            elif k == "ipi_consumed":
                bits, at = owed.get(cpu, (0, 0))
                src = f[0]
                if bits == 0 or src >= NCPU or bits & ((1 << src) | ((1 << src) - 1)) != (1 << src):
                    raise Refusal("ipi_consumption_unbound", r["seq"])
                owed[cpu] = (bits & ~(1 << src), at)
                had = took.get((src, cpu), 0)
                if f[1] <= had:
                    raise Refusal("ipi_duplicate_attribution", r["seq"])
                if f[1] != had + 1:
                    raise Refusal("ipi_consumption_unbound", r["seq"])
                took[(src, cpu)] = f[1]
                c["consumed"] += 1
                cons.append((src, cpu, f[1], at))
        for cpu in range(NCPU):
            if owed.get(cpu, (0, 0))[0]:
                raise Refusal("ipi_consumption_unbound", owed[cpu][1])
        for src, dst, k, at in cons:
            if k > opened.get((src, dst), 0):
                raise Refusal("ipi_unexplained_arrival", at)
        for r in recs:
            f = r["f"]
            if r["kind"] == "ipi_published" and f[2] in (PUB_WAKE, PUB_KICK) and f[3] == 0 \
                    and f[4] > took.get((r["cpu"] & 63, f[0] & 63), 0):
                raise Refusal("ipi_undelivered", r["seq"])
        for cpu in range(NCPU):
            if taken.get(cpu, 0) > requests.get(cpu, 0):
                raise Refusal("ipi_arrivals_exceed_requests", cpu)
        return c

    def consumer_of(p):
        """The arrival that consumed publication p's group, and whether p opened it (fresh)."""
        pb = recs[p]
        if pb["kind"] != "ipi_published" or pb["f"][4] == 0:
            return None
        dst = pb["f"][0]
        c = next((i for i, r in enumerate(recs) if r["kind"] == "ipi_consumed" and r["cpu"] == dst
                  and r["f"][0] == pb["cpu"] and r["f"][1] == pb["f"][4]), None)
        if c is None:
            return None
        a = next((i for i in range(c - 1, -1, -1) if recs[i]["cpu"] == dst and recs[i]["kind"] != "ipi_consumed"), None)
        if a is None or recs[a]["kind"] != "ipi_arrived" or not recs[a]["f"][0] & (1 << (pb["cpu"] & 63)):
            return None
        return a, pb["f"][3] == 0

    def wake_chain(after, frm, to_cpu):
        p = need(find(after, lambda r: r["kind"] == "ipi_published" and r["cpu"] == frm["cpu"] and r["f"][0] == to_cpu
                      and r["f"][2] == PUB_WAKE), "ipi_not_published")
        arrived, fresh = need(consumer_of(p), "ipi_not_consumed")
        if arrived <= after:
            raise Refusal("ipi_attribution_inconsistent")
        return arrived, fresh

    def parked_wake(after, frm, to_cpu, woken):
        arrived, fresh = wake_chain(after, frm, to_cpu)
        if not fresh:
            return after, "merged"
        a = recs[arrived]
        if arr_origin(a) == ORIGIN_IDLE:
            d = need(find(arrived + 1, lambda r: r["cpu"] == to_cpu and r["kind"] in ("idle_dispatch", "ipi_arrived")),
                     "ipi_dispatch_missing")
            dr = recs[d]
            if dr["kind"] != "idle_dispatch" or dr["f"][1] != 1 or dr["f"][0] == 0:
                raise Refusal("ipi_dispatch_substituted")
            return d, ("ipi" if dr["f"][0] == woken["tid"] else "preceded")
        timer = next((i for i in range(after, arrived) if recs[i]["kind"] == "idle_dispatch" and recs[i]["cpu"] == to_cpu
                      and recs[i]["f"][1] == 0 and recs[i]["f"][0] == woken["tid"]), None)
        if timer is not None:
            if arr_tid(a) == woken["tid"]:
                return timer, "timer"
            raise Refusal("ipi_arrived_in_another_task")
        return after, "busy"

    def match_vm_op(begin):
        b = recs[begin]
        if b["kind"] != "vm_op_begin":
            return "not_a_begin"
        found = None
        for i in range(begin + 1, n):
            r = recs[i]
            if r["cpu"] != b["cpu"]:
                continue
            if r["kind"] == "vm_op_begin" and found is None:
                return "incomplete"
            if r["kind"] == "vm_op_end" and r["f"][0] == b["f"][0] and r["f"][1] == b["f"][1] and r["f"][2] == b["f"][3]:
                if found is not None:
                    return "duplicate"
                found = i
        if found is None:
            return "incomplete"
        if recs[found]["f"][3] != 0 or recs[found]["f"][4] != b["f"][1]:
            return "failed"
        return found

    def match_fence(req):
        q = recs[req]
        if q["kind"] != "fence_request":
            return "missing"
        found = None
        for i in range(req + 1, n):
            r = recs[i]
            if r["kind"] != "fence_done" or r["cpu"] != q["cpu"]:
                continue
            if r["f"][:3] != q["f"][:3]:
                continue
            if r["f"][3] != q["f"][3]:
                if found is None:
                    return "stale"
                continue
            if found is not None:
                return "duplicate"
            found = i
        if found is None:
            return "missing"
        if recs[found]["f"][4] != 0:
            return "failed"
        return found

    def check_shoot_decision(shoot, requester):
        """The owner's decision against the production policy, over the inputs it recorded."""
        s = recs[shoot]
        if s["kind"] != "shoot_targets" or s["cpu"] != requester:
            raise Refusal("shoot_targets_missing")
        inputs = next((i for i in range(shoot - 1, -1, -1) if recs[i]["cpu"] == s["cpu"]), None)
        if inputs is None or recs[inputs]["kind"] != "shoot_inputs" or recs[inputs]["f"][:2] != s["f"][:2]:
            raise Refusal("shoot_inputs_missing")
        x = recs[inputs]["f"]
        if U64_MAX in x[2:] or s["f"][3] == U64_MAX or s["f"][4] == U64_MAX:
            raise Refusal("shoot_inputs_unrecorded")
        candidates = (x[2] & 0xFFFFFFFF) & ~(x[2] >> 32)
        placement, loaded = x[3], x[4]
        if (placement | loaded) & ~candidates:
            raise Refusal("shoot_inputs_outside_candidates")
        req_bit = shl(requester) or 0
        mask = s["f"][2]
        if mask & req_bit:
            raise Refusal("shoot_targets_named_requester")
        if mask != (placement | loaded) & ~req_bit:
            raise Refusal("shoot_targets_not_the_policy")
        for cpu in (0, 1):
            bit = 1 << cpu
            if not candidates & bit:
                continue
            cur = s["f"][3 + cpu]
            placed = bool(placement & bit)
            if cur == 0:
                want = False
            else:
                role = next((r for r in known if r["tid"] == cur), None)
                want = None if role is None else role["asid"] == s["f"][0]
            if want is not None and want != placed:
                raise Refusal("shoot_placement_inconsistent")
        return inputs

    def replacement(start, req, target):
        b = need(find(start, lambda r: r["kind"] == "vm_op_begin" and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
                      and r["f"][1] == W and r["f"][4] == req["tid"]), "operation_missing")
        e = match_vm_op(b)
        if e == "failed":
            raise Refusal("operation_failed")
        if e == "duplicate":
            raise Refusal("operation_duplicate_completion")
        if isinstance(e, str):
            raise Refusal("operation_incomplete")

        def inside(kind, after, extra=lambda r: True):
            i = find(after, lambda r: r["kind"] == kind and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
                     and r["f"][1] == W and extra(r))
            return i if i is not None and i < e else None
        disp = need(inside("vm_displaced", b), "displacement_outside_operation")
        sh = need(inside("shoot_targets", disp), "shoot_targets_missing")
        check_shoot_decision(sh, req["cpu"])
        s = recs[sh]
        mask = s["f"][2]
        if target["cpu"] not in (0, 1):
            raise Refusal("shoot_targets_cpu_unrecorded")
        fenced = bool((mask >> (target["cpu"] & 63)) & 1)
        resident = s["f"][3 + target["cpu"]] == target["tid"]
        d = None
        if mask != 0:
            fr = need(inside("fence_request", sh), "fence_not_requested_for_the_targets")
            q = recs[fr]["f"]
            if q[2] & (shl(req["hart"]) or 0):
                raise Refusal("requester_fenced_itself")
            if q[4] != mask:
                raise Refusal("fence_targets_not_the_decision")
            if q[2] != hart_mask_of(mask):
                raise Refusal("fence_harts_not_the_targets")
            d = match_fence(fr)
            if isinstance(d, str):
                raise Refusal({"failed": "fence_failed", "stale": "fence_stale_completion",
                               "duplicate": "fence_duplicate_completion"}.get(d, "fence_completion_missing"))
            if d > e:
                raise Refusal("fence_completion_outside_operation")
        sd = need(inside("vm_shootdown", d if d is not None else sh), "shootdown_outside_operation")
        if recs[sd]["f"][2] != 1:
            raise Refusal("shootdown_not_acknowledged")
        old = recs[disp]["f"][2]
        need(inside("vm_settled", sd, lambda r: r["f"][2] == old), "settlement_outside_operation")
        return dict(b=b, e=e, disp=disp, sh=sh, d=d, fenced=fenced, resident=resident)

    def reactivated(target, disp, before):
        i = find(disp + 1, lambda r: r["kind"] == "activation" and r["cpu"] == target["cpu"] and r["f"][0] == target["asid"])
        return i is not None and i < before

    # ── the verification, in the kernel verifier's order ──
    for r in recs:
        if r["kind"] == "user" and r["f"][2] >= STEP_FAIL:
            fail("user_step_failed", r["seq"])
    try:
        c = population()
        for k in ("published", "fresh", "requests", "arrivals", "consumed", "empty", "merged"):
            cnt[k] = c[k]
    except Refusal as x:
        fail(x.code, x.at, "the IPI population does not balance")
    for r in recs:
        if r["kind"] == "ipi_published" and r["f"][2] in (PUB_WAKE, PUB_KICK) \
                and hart_mask_of(shl(r["f"][0]) or 0) != shl(r["f"][1]):
            fail("ipi_publication_wrong_hart", r["seq"], "publication to cpu %d names hart %d" % (r["f"][0], r["f"][1]))
    for r in recs:
        if r["kind"] == "ipi_arrived" and arr_origin(r) == ORIGIN_USER:
            if r["f"][4] & SSTATUS_FS_VS:
                fail("fp_or_vector_on_in_user", r["seq"], "sstatus 0x%x" % r["f"][4])
            else:
                cnt["user_fp_vs_off"] += 1

    # P1 — parked targets, both directions, every round accounted for.
    at = 0
    route_key = dict(ipi=None, timer="p1_timer_first", busy="p1_busy", preceded="p1_preceded", merged="p1_merged")
    for k in range(1, P1_ROUNDS + 1):
        try:
            call = need(user(at, C, "C_P1_CALL", k), "p1_call_missing")
            d, to_s = parked_wake(call, C, S["cpu"], S)
            resumed = need(user(d, S, "S_P1_RESUMED", k), "p1_server_resume_missing")
            if recs[resumed]["cpu"] != S["cpu"]:
                raise Refusal("p1_server_resumed_elsewhere")
            reply = need(user(resumed, S, "S_P1_REPLY", k), "p1_reply_missing")
            d, to_c = parked_wake(reply, S, C["cpu"], C)
            back = need(user(d, C, "C_P1_RESUMED", k), "p1_client_resume_missing")
            if recs[back]["cpu"] != C["cpu"]:
                raise Refusal("p1_client_resumed_elsewhere")
        except Refusal as x:
            fail(x.code, k, "P1 round %d" % k)
            continue
        cnt["p1_parked"] += 2
        for route, ipi in ((to_s, "p1_ipi_to_s"), (to_c, "p1_ipi_to_c")):
            cnt[route_key[route] or ipi] += 1
        at = back
    if cnt["p1_ipi_to_s"] < P1_ROUNDS or cnt["p1_ipi_to_c"] < P1_MIN_IPI_TO_C:
        fail("p1_too_few_ipi_driven_parked_dispatches", 0, "%d/%d to CPU 1, %d to CPU 0" % (
            cnt["p1_ipi_to_s"], P1_ROUNDS, cnt["p1_ipi_to_c"]))

    # P2 — attempts at an IPI taken in a resident user target.
    def p2_attempt(frm, waker, target, call, sent, ok, window, k):
        call_i = need(user(frm, waker, call, k), "p2_call_missing")
        rd = need(ready(call_i, waker["cpu"], target["cpu"], PHASE_P2, k), "p2_ready_missing")
        sent_i = need(user(rd, waker, sent, k), "p2_sent_missing")
        r = recs[rd]
        if not helper_parked(r):
            ok_i = need(user(rd, target, ok, k), "p2_window_check_missing")
            if recs[ok_i]["cpu"] != target["cpu"]:
                raise Refusal("p2_window_checked_elsewhere")
            return ok_i, False, "helper_not_parked", "not_owed"
        p = find(rd, lambda x: x["kind"] == "ipi_published" and x["cpu"] == waker["cpu"] and x["f"][0] == target["cpu"]
                 and x["f"][2] == PUB_WAKE)
        if p is None or p >= sent_i:
            raise Refusal("ipi_not_published")
        arrived, fresh = wake_chain(rd, waker, target["cpu"])
        ok_i = need(user(arrived, target, ok, k), "p2_window_check_missing")
        if recs[ok_i]["cpu"] != target["cpu"]:
            raise Refusal("p2_window_checked_elsewhere")
        a = recs[arrived]
        if arr_entries(a) <= r["f"][2]:
            raise Refusal("p2_entry_history_inconsistent")
        in_target = arr_origin(a) == ORIGIN_USER and arr_tid(a) == target["tid"] and arr_asid(a) == target["asid"]
        cls = "in_window" if in_target and arr_window(a) == window else ("outside" if in_target else "displaced")
        switched = any(x["kind"] == "activation" and x["cpu"] == target["cpu"] and x["f"][0] != target["asid"]
                       for x in recs[call_i + 1:arrived])
        if not ready_met(r) or r["f"][1] != target["tid"]:
            reason = "not_ready"
        elif not fresh:
            reason = "merged_publication"
        elif arr_entries(a) != r["f"][2] + 1:
            reason = "intervening_entry"
        elif switched:
            reason = "switched"
        else:
            reason = "none"
        if reason == "none" and cls != "in_window":
            raise Refusal("ipi_sepc_outside_window" if in_target else "ipi_eligible_arrival_not_in_target")
        return ok_i, reason == "none", reason, cls

    for k in range(1, P2_ATTEMPTS + 1):
        for waker, call, sent, target, ok, window in (
                (C, "C_P2A_CALL", "C_P2A_SENT", S, "S_WIN_A_OK", WINDOW_S_A),
                (S, "S_P2B_CALL", "S_P2B_SENT", C, "C_WIN_B_OK", WINDOW_C_B)):
            cnt["p2_attempts"] += 1
            key = (2, k, target["cpu"])
            try:
                end, eligible, reason, cls = p2_attempt(at, waker, target, call, sent, ok, window, k)
            except Refusal as x:
                attempts[key] = (0, x.code, "failed")
                fail(x.code, k, "P2 attempt %d -> cpu %d" % (k, target["cpu"]))
                continue
            if cls != "not_owed":
                cnt["p2_" + cls] += 1
            if eligible:
                cnt["p2_credited_s" if target is S else "p2_credited_c"] += 1
                attempts[key] = (1, reason, "credited")
            else:
                cnt["p2_uncredited"] += 1
                attempts[key] = (0, reason, "uncredited")
            if target is C:
                at = end
    if cnt["p2_credited_s"] < P2_MIN_CREDITED_PER_DIRECTION or cnt["p2_credited_c"] < P2_MIN_CREDITED_PER_DIRECTION:
        fail("p2_too_few_credited_attempts_per_direction", 0, "%d to S, %d to C" % (cnt["p2_credited_s"], cnt["p2_credited_c"]))

    # P3 — serial remote-fence attempts (odd: S is the target; even: C is).
    def p3_attempt(rnd, t, q, names):
        pre_name, req_name, obs_name = names
        pre = need(user(0, t, pre_name, rnd), "tlb_pre_missing")
        req = need(user(pre, q, req_name, rnd), "tlb_request_step_missing")
        rd = need(ready(req, q["cpu"], t["cpu"], PHASE_P3, rnd), "tlb_ready_missing")
        rep = replacement(rd, q, t)
        observed = need(user(pre, t, obs_name, rnd), "tlb_observation_missing")
        if observed < rep["e"] or (rep["d"] is not None and observed < rep["d"]):
            raise Refusal("tlb_observed_before_completion")
        if recs[observed]["cpu"] != t["cpu"] or recs[pre]["cpu"] != t["cpu"] or recs[req]["cpu"] != q["cpu"]:
            raise Refusal("tlb_roles_off_their_harts")
        c0 = need(residency(t, pre_name, rnd), "tlb_residency_missing")
        c1 = need(residency(t, obs_name, rnd), "tlb_residency_missing")
        if c1 <= c0:
            raise Refusal("tlb_residency_count_not_monotone")
        if not rep["fenced"] and not reactivated(t, rep["disp"], observed):
            raise Refusal("off_cpu_target_not_reactivated")
        r = recs[rd]
        if not ready_met(r) or r["f"][1] != t["tid"]:
            return "not_ready"
        if not rep["fenced"]:
            return "off_cpu"
        if not rep["resident"]:
            return "not_current"
        if c1 != c0 + 1:
            return "interfered"
        return "none"

    for rnd in range(1, TLB_ROUNDS + 1):
        t, q, names = (S, C, ("S_PRE", "C_REQ", "S_OBSERVED")) if rnd % 2 else (C, S, ("C_PRE", "S_REQ", "C_OBSERVED"))
        key = (3, rnd, t["cpu"])
        try:
            reason = p3_attempt(rnd, t, q, names)
        except Refusal as x:
            attempts[key] = (0, x.code, "failed")
            fail(x.code, rnd, "fence attempt %d (target cpu %d)" % (rnd, t["cpu"]))
            continue
        cnt["tlb_rounds"] += 1
        if reason == "none":
            cnt["credited_s" if t is S else "credited_c"] += 1
            attempts[key] = (1, reason, "credited")
        else:
            cnt[reason if reason in ("interfered", "off_cpu", "not_current") else "not_ready"] += 1
            attempts[key] = (0, reason, "uncredited")
    if cnt["credited_s"] < TLB_MIN_CREDITED_PER_DIRECTION or cnt["credited_c"] < TLB_MIN_CREDITED_PER_DIRECTION:
        fail("tlb_too_few_credited_rounds_per_direction", 0, "%d with S, %d with C" % (cnt["credited_s"], cnt["credited_c"]))

    # P4 — mutual rounds: obligations, all of them.
    def mutual_round(m):
        sn = user(0, S, "S_MUT_NR3", m)
        cn = user(0, C, "C_MUT_NR3", m)
        if sn is None or cn is None:
            raise Refusal("mut_request_missing")
        try:
            s_op = replacement(sn, S, C)
        except Refusal:
            raise Refusal("mut_s_operation_invalid")
        try:
            c_op = replacement(cn, C, S)
        except Refusal:
            raise Refusal("mut_c_operation_invalid")
        so = user(0, S, "S_MUT_OK", m)
        co = user(0, C, "C_MUT_OK", m)
        if so is None or co is None:
            raise Refusal("mut_observation_missing")
        if so < c_op["e"] or co < s_op["e"]:
            raise Refusal("mut_observed_before_operation_completed")
        if not s_op["fenced"] and not reactivated(C, s_op["disp"], co):
            raise Refusal("off_cpu_target_not_reactivated")
        if not c_op["fenced"] and not reactivated(S, c_op["disp"], so):
            raise Refusal("off_cpu_target_not_reactivated")
        first, last = min(s_op["b"], c_op["b"]), max(s_op["e"], c_op["e"])
        c_in = find(first, lambda r: r["kind"] == "contention" and r["f"][1] == 0)
        c_out = find(last, lambda r: r["kind"] == "contention" and r["f"][1] == 1)
        contended = max(recs[c_out]["f"][0] - recs[c_in]["f"][0], 0) if c_in is not None and c_out is not None else 0
        overlapped = max(s_op["b"], c_op["b"]) < min(s_op["e"], c_op["e"])
        return s_op, c_op, overlapped, contended

    for m in range(1, MUT_ROUNDS + 1):
        try:
            s_op, c_op, ov, contended = mutual_round(m)
        except Refusal as x:
            attempts[(4, m, 0)] = (0, x.code, "failed")
            fail(x.code, m, "mutual round %d" % m)
            continue
        cnt["mutual_rounds"] += 1
        cnt["overlapped"] += int(ov)
        cnt["contended"] += contended
        for op, tgt in ((s_op, C), (c_op, S)):
            cnt["mutual_off_cpu"] += int(not op["fenced"])
            cnt["mutual_not_current"] += int(op["fenced"] and not op["resident"])
            reason = "off_cpu" if not op["fenced"] else ("not_current" if not op["resident"] else "resident")
            attempts[(4, m, tgt["cpu"])] = (1, reason, "verified")
        if not ov:
            fail("mut_operations_serialized", m, "mutual round %d" % m)

    # Reclaim only after the shootdown the decision owed completed, exactly once, per displaced page.
    for i, d in enumerate(recs):
        if d["kind"] != "vm_displaced" or d["f"][1] != W:
            continue
        st = [j for j in range(i + 1, n) if recs[j]["kind"] == "vm_settled" and recs[j]["f"][:3] == d["f"][:3]]
        if len(st) != 1:
            fail("settlement_not_exactly_once", d["seq"])
            continue

        def same(x, kind):
            return x["kind"] == kind and x["cpu"] == d["cpu"] and x["f"][:2] == d["f"][:2]
        sh = next((j for j in range(i + 1, st[0]) if same(recs[j], "shoot_targets")), None)
        if sh is None:
            fail("settled_without_shootdown_targets", d["seq"])
            continue
        try:
            check_shoot_decision(sh, d["cpu"])
        except Refusal as x:
            fail(x.code, d["seq"], "the settled page's shootdown decision")
            continue
        ack = next((j for j in range(sh + 1, st[0]) if same(recs[j], "vm_shootdown") and recs[j]["f"][2] == 1), None)
        if ack is None:
            fail("settled_before_shootdown_acknowledged", d["seq"])
            continue
        mask = recs[sh]["f"][2]
        if mask == 0:
            cnt["settled_local_only"] += 1
            continue
        want = hart_mask_of(mask)
        done = [j for j in range(sh + 1, ack) if same(recs[j], "fence_request") and recs[j]["f"][4] == mask
                and recs[j]["f"][2] == want and not isinstance(match_fence(j), str) and match_fence(j) < ack]
        if done:
            cnt["settled_after_completion"] += 1
        else:
            fail("settled_before_fence_completion", d["seq"])
    if cnt["tlb_rounds"] + cnt["mutual_rounds"] * 2 > cnt["settled_after_completion"] + cnt["settled_local_only"]:
        fail("displaced_pages_unsettled", 0)
    if user(0, S, "S_DONE") is None or user(0, C, "C_DONE") is None:
        fail("witness_not_complete", 0)
    return fails, cnt, attempts


REC_RE = re.compile(r"SMP3_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) "
                    r"f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)(?: step=(\w+))?$")
ROLES_RE = re.compile(r"SMP3_ROLES s=(\d+):(\d+):(\d+):(\d+) c=(\d+):(\d+):(\d+):(\d+) h1=(\d+):(\d+) h0=(\d+):(\d+) w_va=0x([0-9a-f]+)")


def parse_rec(body):
    m = REC_RE.match(body)
    if not m:
        return None
    return dict(seq=int(m.group(1)), kind=m.group(2), cpu=int(m.group(3)),
                f=[int(m.group(i), 16) for i in range(4, 9)], step=m.group(9))


def parse_roles(body):
    m = ROLES_RE.search(body or "")
    if not m:
        return None
    g = [int(x) for x in m.groups()[:12]]
    return dict(s=dict(tid=g[0], asid=g[1], cpu=g[2], hart=g[3]),
                c=dict(tid=g[4], asid=g[5], cpu=g[6], hart=g[7]),
                h1=dict(tid=g[8], asid=g[9]), h0=dict(tid=g[10], asid=g[11]), w=int(m.group(13), 16))


def main_records(path):
    """Records only: grade one SMP3_ROLES line and its SMP3_REC lines, and print the verdict, the
    counts and every attempt in a form the differential test compares with the kernel verifier."""
    roles, recs = None, []
    for raw in open(path).read().splitlines():
        body = re.sub(r" pass=[12] crc=0x[0-9a-f]{8}$", "", raw.strip())
        if body.startswith("SMP3_ROLES "):
            roles = parse_roles(body)
        elif body.startswith("SMP3_REC "):
            r = parse_rec(body)
            if r is None:
                print("SMP3_RECORDS_MALFORMED %r" % body)
                return 2
            recs.append(r)
    if roles is None or [r["seq"] for r in recs] != list(range(len(recs))):
        print("SMP3_RECORDS_MALFORMED roles=%s contiguous=%s" % (roles is not None, [r["seq"] for r in recs] == list(range(len(recs)))))
        return 2
    fails, cnt, attempts = verify_records(recs, roles)
    first = fails[0] if fails else ("none", 0, "")
    print("SMP3_RECORDS_VERDICT result=%s reason=%s at=%d" % ("fail" if fails else "ok", first[0], first[1]))
    print("SMP3_RECORDS_COUNTS " + " ".join("%s=%d" % (k, cnt[k]) for k in COUNT_KEYS))
    for key in sorted(attempts):
        e, reason, outcome = attempts[key]
        print("SMP3_RECORDS_ATTEMPT phase=%d n=%d target_cpu=%d eligible=%d reason=%s outcome=%s" % (key + (e, reason, outcome)))
    for code, at, detail in fails:
        print("SMP3_RECORDS_FAILURE reason=%s at=%d %s" % (code, at, detail))
    return 1 if fails else 0


def main_log(argv):
    text = open(argv[1], "rb").read().decode("utf-8", "replace").replace("\r", "\n")
    lines = text.split("\n")
    fails = []

    def fail(msg):
        fails.append(msg)

    if argv[2] != "0":
        fail("the built image's fence / publication / clear ordering is incomplete (sequence.txt)")

    # ── FIRMWARE AND BRING-UP ──
    sbi = next((l for l in lines if "Runtime SBI Version" in l), None)
    osbi = next((l for l in lines if re.search(r"OpenSBI v\d", l)), None)
    if not sbi or not osbi:
        fail("the OpenSBI banner (version, runtime SBI version) is missing")

    # QEMU-SMP3-ACCEPTANCE §3 — THE QUALIFIED FIRMWARE. The remote-fence completion claim holds for
    # one pinned implementation, so every boot must prove it ran exactly that one: the runner's
    # recorded artifact, the banner, and the firmware's own Base-extension answer. Any missing piece
    # fails.
    pin_path = argv[3] if len(argv) > 3 else None
    ident_path = argv[4] if len(argv) > 4 else None
    if not pin_path or not ident_path:
        fail("no firmware pin / artifact identity was given: the firmware is unqualified")
    else:
        pin = dict(l.split("=", 1) for l in open(pin_path).read().splitlines() if "=" in l and not l.startswith("#"))
        try:
            ident = open(ident_path).read()
        except OSError:
            ident = ""
        m = re.search(r"^firmware_sha256=([0-9a-f]{64})$", ident, re.M)
        if not m:
            fail("the run recorded no firmware identity")
        elif m.group(1) != pin["sha256"]:
            fail("the booted firmware %s is not the pinned %s" % (m.group(1), pin["sha256"]))
        if osbi and pin["banner"] not in osbi:
            fail("the banner names %r, not the pinned %r" % (osbi.strip(), pin["banner"]))
        if sbi and not re.search(r"Runtime SBI Version\s*:\s*%s\b" % re.escape(pin["runtime_sbi"]), sbi):
            fail("the runtime SBI version is not the pinned %s" % pin["runtime_sbi"])
        ids = [re.search(r"SMP3_SBI_IDENTITY spec=0x([0-9a-f]+) impl_id=(\d+) impl_version=0x([0-9a-f]+)", l) for l in lines]
        ids = [x for x in ids if x]
        if len(ids) != 1:
            fail("expected exactly one firmware self-identification, saw %d" % len(ids))
        else:
            spec, impl_id, impl_ver = int(ids[0].group(1), 16), int(ids[0].group(2)), int(ids[0].group(3), 16)
            if (spec, impl_id, impl_ver) != (int(pin["spec"], 16), int(pin["impl_id"]), int(pin["impl_version"], 16)):
                fail("the firmware identifies as spec=0x%x impl_id=%d impl_version=0x%x, not the pinned implementation" % (spec, impl_id, impl_ver))
            else:
                print("[smp3-witness] firmware pinned: sha256=%s impl_id=%d impl_version=0x%x spec=0x%x" % (pin["sha256"][:16], impl_id, impl_ver, spec))
    # Printed while only one hart writes the console (the secondary is parked, or not yet released),
    # so each must be present exactly once and intact.
    for pat, want in [
        ("RISCV_SECONDARY_PARK_WAKE_SOURCE hart=", 1),
        ("RISCV_SMP3_RELEASE boot_cpu=0 ", 1),
        ("SMP3_WITNESS_PROVISIONED ", 1),
    ]:
        k = sum(pat in l for l in lines)
        if k != want:
            fail("expected %d x '%s', saw %d" % (want, pat, k))
    # Printed by the released secondary while the boot hart is also writing. The two harts share the
    # console without a line lock, so these can be interleaved byte-wise: an intact copy is checked,
    # at most one may exist, and the graded facts come from the sealed `SMP3_BRINGUP` line.
    live_damaged = []
    for pat in ["RISCV_SMP3_SECONDARY_RELEASED ", "RISCV_SMP3_SECONDARY_ADMITTED ",
                "SMP3_WITNESS_SECONDARY_PLACED "]:
        k = sum(pat in l for l in lines)
        if k > 1:
            fail("expected at most 1 x '%s', saw %d" % (pat, k))
        elif k == 0:
            live_damaged.append(pat.strip())
    wake = next((l for l in lines if "RISCV_SECONDARY_PARK_WAKE_SOURCE" in l), "")
    if "ssie=1 sstatus_sie=0 delivery=wfi_wake_only" not in wake:
        fail("the park's wake source is not SSIE-only with SIE clear: %r" % wake)
    rel = next((l for l in lines if "RISCV_SMP3_RELEASE boot_cpu=0" in l), "")
    if "ssie=1" not in rel or "released_harts=1 refused=0 knob=1" not in rel:
        fail("the boot hart's release is not one successful knob-gated release: %r" % rel)
    adm = next((l for l in lines if "RISCV_SMP3_SECONDARY_ADMITTED" in l), None)
    if adm is not None and ("cpu=1 wake_only=0 balance_excluded=1 idle_placeholder=cleared ipi_ready=1" not in adm or "result=ok" not in adm):
        fail("the secondary's admission is not the one scheduler transition: %r" % adm)
    if any(p in l for l in lines for p in ("RISCV_SMP3_SECONDARY_UNADMITTED", "RISCV_SMP3_SECONDARY_ADMISSION_REFUSED")):
        fail("the secondary was not admitted")
    m = re.search(r"RISCV_BOOT_HART_SELECTED hart=(\d+)", text)
    boot_hart = int(m.group(1)) if m else None

    # ── NOTHING FATAL ──
    for bad in ["PANIC", "panicked", "SMP3_USER_FAIL", "SMP3_WITNESS_PROVISION_FAIL",
                "RISCV_SMP3_SECONDARY_UNADMITTED", "RISCV_SMP3_SECONDARY_ADMISSION_REFUSED",
                "SMP3_WITNESS_SECONDARY_PLACEMENT_FAIL", "_BROAD_ENTRY", "_UNROUTED",
                "RISCV_TRAP_UNHANDLED", "RISCV_TRAP_HANDLE_FAILED", "IRQ1_UNKNOWN_SETTLED",
                "trap_from_s_mode"]:
        k = sum(bad in l for l in lines)
        if k:
            fail("%d line(s) with %s" % (k, bad))

    # ── the sealed record: each line taken only from a copy whose checksum verifies ──
    def fnv1a(b):
        h = 0x811C9DC5
        for x in b:
            h = ((h ^ x) * 0x01000193) & 0xFFFFFFFF
        return h

    keys = r"SMP3_(REC|ATTEMPT|ROLES|BRINGUP|CPU_IPI|CPU_FENCE|COUNTS_IPI|COUNTS_WAKE|COUNTS_P2|COUNTS_TLB|COUNTS_MUT|SYNC|SEAL_SYNC|EVID_SYNC|VERDICT) "
    dump, damaged = {}, 0
    for l in lines:
        i, j = l.find("SMP3_"), l.rfind(" pass=")
        m = re.search(r" pass=([12]) crc=0x([0-9a-f]{8})$", l.rstrip())
        if i < 0 or j < i or not m or not re.match(keys, l[i:]):
            if " pass=" in l and "crc=" in l:
                damaged += 1
            continue
        body = l[i:j]
        if fnv1a(body.encode()) != int(m.group(2), 16):
            damaged += 1
            continue
        key = body.split(" ", 1)[0]
        if key == "SMP3_REC":
            key = "REC " + re.search(r"seq=(\d+)", body).group(1)
        elif key == "SMP3_ATTEMPT":
            key = "ATTEMPT " + " ".join(re.search(r"phase=(\d+) n=(\d+) target_cpu=(\d+)", body).groups())
        elif key.startswith("SMP3_CPU_"):
            key = key + " " + body.split(" ", 2)[1]
        dump.setdefault(key, body)

    roles = parse_roles(dump.get("SMP3_ROLES"))
    counts = " ".join(v for k, v in sorted(dump.items()) if k.startswith("SMP3_COUNTS"))
    # ── the secondary's admitted state and placement, from the sealed line ──
    # The secondary's hart as the park itself announced it (printed before the release, so intact) —
    # independent of the kernel's CPU->hart table that the sealed lines are written from.
    m = re.search(r"RISCV_SECONDARY_PARK_WAKE_SOURCE hart=(\d+) cpu=1 ", text)
    sec_hart = int(m.group(1)) if m else None
    m = re.search(r"SMP3_BRINGUP cpu=1 hart=(\d+) sie=0x([0-9a-f]+) sstatus_sie=(\d) sum=(\d) placed=(\w+) tids=9302,9300 kick=(\d)$", dump.get("SMP3_BRINGUP", ""))
    if not m:
        fail("the sealed bring-up line is missing or malformed: %r" % dump.get("SMP3_BRINGUP"))
    else:
        if sec_hart is None or int(m.group(1)) != sec_hart:
            fail("the sealed bring-up hart %s is not the parked secondary's hart %s" % (m.group(1), sec_hart))
        sie = int(m.group(2), 16)
        # Admitted with SSIE as the only enabled source, SIE clear outside the idle wait, SUM set.
        if sie != 0x2 or m.group(3) != "0" or m.group(4) != "1":
            fail("the secondary's admitted state is wrong: sie=0x%x sstatus_sie=%s sum=%s" % (sie, m.group(3), m.group(4)))
        if m.group(5) != "ok" or m.group(6) != "1":
            fail("the secondary's two tasks were not placed and kicked: placed=%s kick=%s" % (m.group(5), m.group(6)))
    verdict = dump.get("SMP3_VERDICT")
    nrec = re.search(r"records=(\d+)", counts)
    recs = []
    if nrec:
        for q in range(int(nrec.group(1))):
            body = dump.get("REC %d" % q)
            if body is None:
                fail("record seq %d damaged in both dump copies or missing" % q)
                continue
            r = parse_rec(body)
            if r is None:
                fail("record seq %d malformed: %r" % (q, body))
                continue
            recs.append(r)
    if roles is None or not recs or verdict is None or not nrec:
        fail("record not dumped (roles=%s records=%d verdict=%s)" % (roles is not None, len(recs), verdict is not None))
        for f in fails:
            print("[smp3-witness][fail] " + f)
        print("SMP3_WITNESS_SEAL result=fail")
        return 1
    if [r["seq"] for r in recs] != list(range(len(recs))):
        fail("record sequence is not contiguous from 0")
    S, C = roles["s"], roles["c"]
    if (S["cpu"], C["cpu"]) != (1, 0):
        fail("roles not on their home CPUs")
    # Hart identity: the roles' harts are the harts the boot actually ran, never CPU indices assumed.
    if sec_hart is None or boot_hart is None or S["hart"] != sec_hart or C["hart"] != boot_hart or sec_hart == boot_hart:
        fail("hart identity: s_hart=%s (secondary %s), c_hart=%s (boot %s)" % (S["hart"], sec_hart, C["hart"], boot_hart))

    # ── the record-level verification (every obligation, then coverage) ──
    vfails, cnt, attempts = verify_records(recs, roles)
    for code, at, detail in vfails:
        fail("%s at=%d%s" % (code, at, (": " + detail) if detail else ""))

    # ── CONTEXT ──
    for step, want in {"S_P1_RESUMED": P1_ROUNDS, "C_P1_RESUMED": P1_ROUNDS, "S_WIN_A_OK": P2_ATTEMPTS, "C_WIN_B_OK": P2_ATTEMPTS,
                       "S_OBSERVED": TLB_ROUNDS // 2, "C_OBSERVED": TLB_ROUNDS // 2, "S_MUT_OK": MUT_ROUNDS, "C_MUT_OK": MUT_ROUNDS,
                       "S_DONE": 1, "C_DONE": 1}.items():
        k = sum(r["kind"] == "user" and r["f"][2] == CODE[step] for r in recs)
        if k != want:
            fail("step %s seen %d time(s), want %d" % (step, k, want))

    # ── the kernel verifier must agree: every attempt line, then every count ──
    kernel_attempts = {}
    for k, body in dump.items():
        if not k.startswith("ATTEMPT "):
            continue
        m = re.match(r"SMP3_ATTEMPT phase=(\d+) n=(\d+) target_cpu=(\d+) gen=0x[0-9a-f]+ eligible=([01]) reason=(\w+) outcome=(\w+)$", body)
        if not m:
            fail("malformed kernel attempt line: %r" % body)
            continue
        kernel_attempts[(int(m.group(1)), int(m.group(2)), int(m.group(3)))] = "%s %s %s" % m.group(4, 5, 6)
    graded = dict((key, "%d %s %s" % v) for key, v in attempts.items())
    for key in sorted(set(graded) | set(kernel_attempts)):
        if graded.get(key) != kernel_attempts.get(key):
            fail("attempt phase=%d n=%d target_cpu=%d: grader %s, kernel %s" % (key + (graded.get(key), kernel_attempts.get(key))))
    for k in COUNT_KEYS:
        if "%s=%d" % (k, cnt[k]) not in counts.split():
            fail("kernel counts disagree on %s=%d: %s" % (k, cnt[k], counts))
    if not verdict.startswith("SMP3_VERDICT result=ok "):
        fail("kernel verdict: %s" % verdict)

    summary = ("records=%d damaged_lines=%d published=%d fresh=%d merged=%d requests=%d arrivals=%d consumed=%d empty=%d "
               "p1=%d ipi_to_s=%d ipi_to_c=%d timer_first=%d busy=%d preceded=%d p1_merged=%d p2_attempts=%d "
               "p2_credited_s=%d p2_credited_c=%d p2_uncredited=%d p2_outside=%d p2_displaced=%d fence_rounds=%d "
               "credited_s=%d credited_c=%d interfered=%d off_cpu=%d not_ready=%d not_current=%d mutual=%d overlapped=%d "
               "mutual_off_cpu=%d mutual_not_current=%d contended=%d settled=%d settled_local=%d") % (
        len(recs), damaged, cnt["published"], cnt["fresh"], cnt["merged"], cnt["requests"], cnt["arrivals"], cnt["consumed"],
        cnt["empty"], cnt["p1_parked"], cnt["p1_ipi_to_s"], cnt["p1_ipi_to_c"], cnt["p1_timer_first"], cnt["p1_busy"],
        cnt["p1_preceded"], cnt["p1_merged"], cnt["p2_attempts"], cnt["p2_credited_s"], cnt["p2_credited_c"],
        cnt["p2_uncredited"], cnt["p2_outside"], cnt["p2_displaced"], cnt["tlb_rounds"], cnt["credited_s"], cnt["credited_c"],
        cnt["interfered"], cnt["off_cpu"], cnt["not_ready"], cnt["not_current"], cnt["mutual_rounds"], cnt["overlapped"],
        cnt["mutual_off_cpu"], cnt["mutual_not_current"], cnt["contended"], cnt["settled_after_completion"],
        cnt["settled_local_only"])
    print("[smp3-witness] " + summary)
    print("[smp3-witness] kernel: " + counts + " | " + verdict)
    print("[smp3-witness] " + dump.get("SMP3_SYNC", "SMP3_SYNC missing"))
    print("[smp3-witness] " + dump.get("SMP3_SEAL_SYNC", "SMP3_SEAL_SYNC missing"))
    print("[smp3-witness] " + dump.get("SMP3_EVID_SYNC", "SMP3_EVID_SYNC missing"))
    for key in sorted(graded):
        if graded[key].split()[2] != "credited":
            print("[smp3-witness] attempt phase=%d n=%d target_cpu=%d: %s" % (key + (graded[key],)))
    for k in sorted(dump):
        if k.startswith("SMP3_CPU_"):
            print("[smp3-witness] " + dump[k])
    print("[smp3-witness] firmware: %s | %s" % ((osbi or "").strip(), (sbi or "").strip()))
    print("[smp3-witness] " + dump.get("SMP3_BRINGUP", "SMP3_BRINGUP missing"))
    if live_damaged:
        print("[smp3-witness] live secondary markers interleaved on the shared console (graded from the sealed line): " + ", ".join(live_damaged))
    # Reported, not graded: present at the same rate in the base boots (it scales with boot length).
    print("[smp3-witness] pre-existing RISCV_ASYNC_RESUME_REFUSED lines: %d" % sum("RISCV_ASYNC_RESUME_REFUSED" in l for l in lines))
    print("[smp3-witness] overtaken-deferral settlements: %d" % sum("RISCV_OVERTAKEN_DEFERRAL_SETTLED" in l for l in lines))
    print("[smp3-witness] replies retried while the caller was still blocking: %d" % sum("IPCREPLY_DIRECT_CALLER_NOT_YET_BLOCKED" in l for l in lines))
    # The phase totals, each a named failure rather than a silent seal condition.
    for name, got, want in [("parked-target resumes", cnt["p1_parked"], 2 * P1_ROUNDS), ("fence attempts", cnt["tlb_rounds"], TLB_ROUNDS),
                            ("P2 attempts", cnt["p2_attempts"], 2 * P2_ATTEMPTS),
                            ("mutual rounds", cnt["mutual_rounds"], MUT_ROUNDS), ("overlapped mutual rounds", cnt["overlapped"], MUT_ROUNDS)]:
        if got != want:
            fail("%s: %d, want %d" % (name, got, want))
    for f in fails:
        print("[smp3-witness][fail] " + f)
    ok = not fails
    print("SMP3_WITNESS_SEAL %s result=%s" % (summary, "ok" if ok else "fail"))
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--records":
        sys.exit(main_records(sys.argv[2]))
    if len(sys.argv) == 2 and sys.argv[1] == "--steps":
        for name, code in STEPS:
            print("%s %d" % (name, code))
        sys.exit(0)
    sys.exit(main_log(sys.argv))
