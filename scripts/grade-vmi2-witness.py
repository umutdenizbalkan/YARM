#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION2 dispatch-gap witness grader (SEAL revision).
#
# The verdict is derived from the COMPLETE per-round records (`VMI2_F r= k= v= crc=` lines, each
# emitted twice), never from a trusted summary, and never from a threshold the log declares for
# itself. The qualification contract is pinned HERE: CONTRACT_ROUNDS complete rounds and at least
# CONTRACT_GAP_THRESHOLD credited gaps. Provisioning and seal must both declare exactly that; a
# missing, conflicting or lowered declaration fails.
#
# Grading has two separate stages, in this order:
#
#  1. VALIDITY — every scheduled round, whether or not it will earn gap credit:
#     * a complete, non-conflicting record (two CRC-valid copies that disagree fail; a torn copy is
#       dropped; redundant identical copies are transport redundancy);
#     * identities bound through their producers: the gap owner's observer CPU and `{tid, asid}`
#       incarnation (one observer for every round, not the boot task), the post producer's same
#       incarnation, the shootdown owner's requester CPU (never the observer's CPU), ASID and OBS;
#     * exactly one GENUINE replacement (an old backing displaced by a different new one);
#     * an acknowledged completion with the displaced backing settled after it;
#     * recorded event order: gap_enter < gap_release < post_seq, own_select < own_complete <
#       post_seq, each round's stamps after the previous round's (both programs are sequential),
#       and every stamp unique;
#     * the pre/post values (the prior backing's value before, this round's sentinel after);
#     * the target obligation: the owner's ACTUAL target set names the recorded observer CPU whenever
#       the recorded residency (read beside the owner's decision) still has A loaded there. Inside
#       the gap the observer CPU is spinning with A loaded, so there the residency must say so and
#       the target must name it; outside the gap a CPU that left A (cleared only behind the idle
#       root load and its flush) owes nothing, and is not demanded.
#     Any invalid round fails the boot, regardless of coverage.
#
#  2. COVERAGE — only a VALID round may be credited, and it is credited exactly when the owner's
#     selection was stamped inside the observer's gap: gap_enter < own_select < gap_release. A valid
#     round whose operation missed the gap is "completed, uncredited". Completion need NOT precede
#     gap release (it normally follows it); it must precede the observer's post-read.
#
# The sequence stamps order RECORDED events through one shared atomic counter. They establish the
# order in which the instrumentation observed those events, not architectural memory-barrier or
# TLB-ordering correctness; that rests on the production source and its deterministic tests.

import argparse
import binascii
import os
import re
import subprocess
import sys
import tempfile

# ── the pinned qualification contract ────────────────────────────────────────────────────────────
CONTRACT_ROUNDS = 8
CONTRACT_GAP_THRESHOLD = 4

RES_REPLACED = 1
RES_NO_DISPLACE = 2
DISP_SETTLED = 1
DISP_QUARANTINED = 2
MAX_CPUS = 64

# Every field a complete round record must carry (excluding `round`, which is the key).
REQUIRED_FIELDS = [
    "declared", "pre", "gap_enter", "gap_release", "diag_target",
    "obs_cpu", "obs_tid", "obs_asid",
    "own_cpu", "own_asid", "own_va", "own_old", "own_new", "own_result", "own_target",
    "own_resident", "own_acked", "own_disp", "own_select", "own_complete", "own_writes",
    "refused", "post", "post_seq", "post_cpu", "post_tid", "post_asid",
]
STAMPS = ["gap_enter", "gap_release", "own_select", "own_complete", "post_seq"]

FIELD_RE = re.compile(r"VMI2_F r=(\d+) k=([a-z_]+) v=(\d+) crc=([0-9a-f]{8})")
PROV_RE = re.compile(
    r"VMI2_WITNESS_PROVISIONED asid=(\d+) boot_tid=(\d+) rounds=(\d+) gap_threshold=(\d+) "
    r"obs_va=0x([0-9a-f]+) mbx_phys=0x[0-9a-f]+ image=\d+ seed=0x([0-9a-f]+) "
    r"newval_base=0x([0-9a-f]+)"
)
SEAL_RE = re.compile(
    r"VMI2_WITNESS_SEAL arch=x86_64 rounds=(\d+) gap_threshold=(\d+) seed=0x([0-9a-f]+) "
    r"newval_base=0x([0-9a-f]+) obs_va=0x([0-9a-f]+)"
)
DECL_FRAG_RE = re.compile(r"\b(rounds|gap_threshold)=(\d+)")


def crc32(s: str) -> int:
    return binascii.crc32(s.encode("latin1")) & 0xFFFFFFFF


def collect_fields(text):
    """Return {round: {key: set(values from CRC-valid lines)}}. Torn copies (bad CRC) are dropped."""
    by_round = {}
    for line in text.splitlines():
        m = FIELD_RE.search(line)
        if not m:
            continue
        r, k, v, crc = int(m.group(1)), m.group(2), int(m.group(3)), int(m.group(4), 16)
        if crc32(f"r={r} k={k} v={v}") != crc:
            continue  # a torn/corrupt copy: ignored, never trusted
        by_round.setdefault(r, {}).setdefault(k, set()).add(v)
    return by_round


def read_params(text):
    """Parse and cross-check the provisioning and seal declarations against the pinned contract.

    Returns (params, errors). Every complete copy of each declaration must agree; provisioning and
    seal must agree with each other; and both must declare exactly the pinned contract. A lowered
    or raised rounds/threshold value fails even in a torn fragment of either declaration line.
    """
    errors = []
    provs = {m.groups() for m in PROV_RE.finditer(text)}
    seals = {m.groups() for m in SEAL_RE.finditer(text)}
    if not provs:
        errors.append("declaration: no complete VMI2_WITNESS_PROVISIONED line")
    if not seals:
        errors.append("declaration: no complete VMI2_WITNESS_SEAL line")
    if len(provs) > 1:
        errors.append(f"declaration: {len(provs)} conflicting VMI2_WITNESS_PROVISIONED copies")
    if len(seals) > 1:
        errors.append(f"declaration: {len(seals)} conflicting VMI2_WITNESS_SEAL copies")
    for line in text.splitlines():
        if "VMI2_WITNESS_PROVISIONED" not in line and "VMI2_WITNESS_SEAL" not in line:
            continue
        for key, val in DECL_FRAG_RE.findall(line):
            want = CONTRACT_ROUNDS if key == "rounds" else CONTRACT_GAP_THRESHOLD
            if int(val) != want:
                errors.append(f"declaration: {key}={val} differs from the pinned contract {key}={want}")
    if errors or len(provs) != 1 or len(seals) != 1:
        return None, errors
    (asid, boot_tid, p_rounds, p_thr, p_obs, p_seed, p_new) = next(iter(provs))
    (s_rounds, s_thr, s_seed, s_new, s_obs) = next(iter(seals))
    if (p_rounds, p_thr, p_seed, p_new, p_obs) != (s_rounds, s_thr, s_seed, s_new, s_obs):
        errors.append("declaration: provisioning and seal disagree")
        return None, errors
    params = {
        "asid": int(asid),
        "boot_tid": int(boot_tid),
        "seed": int(p_seed, 16),
        "newval_base": int(p_new, 16),
        "obs_va": int(p_obs, 16),
    }
    return params, errors


def validate_round(r, f, params, observer_tid, prev):
    """Return the list of reasons round `r` is INVALID (empty when valid). Never decides credit."""
    bad = []
    tag = f"round {r}:"
    if f["refused"]:
        bad.append(f"{tag} replacement refused")
    if f["declared"] != 1:
        bad.append(f"{tag} no gap recorded by the gap owner")
    # ── identities, bound through their producers ──
    obs_cpu, own_cpu = f["obs_cpu"], f["own_cpu"]
    if f["obs_tid"] == 0 or f["obs_tid"] == params["boot_tid"]:
        bad.append(f"{tag} observer incarnation tid={f['obs_tid']} missing or the boot task")
    elif observer_tid is not None and f["obs_tid"] != observer_tid:
        bad.append(f"{tag} observer tid={f['obs_tid']} differs from the observer {observer_tid}")
    if f["obs_asid"] != params["asid"]:
        bad.append(f"{tag} observer asid={f['obs_asid']} != witness asid {params['asid']}")
    if f["post_tid"] != f["obs_tid"] or f["post_asid"] != params["asid"]:
        bad.append(f"{tag} post producer {{tid={f['post_tid']}, asid={f['post_asid']}}} is not the "
                   f"observer {{tid={f['obs_tid']}, asid={params['asid']}}}")
    if obs_cpu >= MAX_CPUS or own_cpu >= MAX_CPUS or f["post_cpu"] >= MAX_CPUS:
        bad.append(f"{tag} CPU identity out of range")
        return bad
    if obs_cpu == own_cpu:
        bad.append(f"{tag} requester CPU {own_cpu} is the observer's own CPU")
    if f["own_target"] & (1 << own_cpu):
        bad.append(f"{tag} owner target 0x{f['own_target']:x} names the requester CPU {own_cpu}")
    if f["own_asid"] != params["asid"]:
        bad.append(f"{tag} owner asid={f['own_asid']} != witness asid {params['asid']}")
    if f["own_va"] != params["obs_va"]:
        bad.append(f"{tag} owner va=0x{f['own_va']:x} != OBS 0x{params['obs_va']:x}")
    # ── exactly one genuine replacement ──
    if f["own_writes"] != 1:
        bad.append(f"{tag} owner record writes={f['own_writes']} (want exactly 1)")
    if f["own_result"] == RES_NO_DISPLACE:
        bad.append(f"{tag} install displaced nothing (not a replacement)")
    elif f["own_result"] != RES_REPLACED or f["own_old"] == 0 or f["own_new"] == 0:
        bad.append(f"{tag} no genuine replacement (result={f['own_result']} "
                   f"old=0x{f['own_old']:x} new=0x{f['own_new']:x})")
    elif f["own_old"] == f["own_new"]:
        bad.append(f"{tag} replacement installed the SAME backing 0x{f['own_old']:x}")
    # ── acknowledged completion and the recorded disposition ──
    if f["own_acked"] != 1 and f["own_disp"] == DISP_SETTLED:
        bad.append(f"{tag} displaced backing settled WITHOUT acknowledgement (premature reclaim)")
    elif f["own_acked"] != 1:
        bad.append(f"{tag} shootdown not acknowledged (required invalidation incomplete)")
    elif f["own_disp"] != DISP_SETTLED:
        bad.append(f"{tag} displaced backing not settled (disp={f['own_disp']})")
    # ── recorded event order ──
    ge, gr, sel, comp, post_seq = (f[k] for k in STAMPS)
    if 0 in (ge, gr, sel, comp, post_seq):
        bad.append(f"{tag} an event stamp is missing")
    else:
        if not ge < gr:
            bad.append(f"{tag} gap release {gr} not after gap entry {ge}")
        if not sel < comp:
            bad.append(f"{tag} completion {comp} does not follow target selection {sel}")
        if not comp < post_seq:
            bad.append(f"{tag} observer post-read {post_seq} does not follow completion {comp}")
        if not gr < post_seq:
            bad.append(f"{tag} observer post-read {post_seq} inside its own gap (release {gr})")
        if prev is not None:
            if not prev["post_seq"] < ge:
                bad.append(f"{tag} gap entry {ge} precedes round {r - 1}'s post-read {prev['post_seq']}")
            if not prev["own_complete"] < sel:
                bad.append(f"{tag} selection {sel} precedes round {r - 1}'s completion {prev['own_complete']}")
    # ── pre/post values ──
    want_pre = params["seed"] if r == 1 else params["newval_base"] + (r - 1)
    want_post = (params["newval_base"] + r) & 0xFFFFFFFFFFFFFFFF
    if f["pre"] != want_pre:
        bad.append(f"{tag} pre 0x{f['pre']:x} != expected 0x{want_pre:x}")
    if f["post"] != want_post:
        bad.append(f"{tag} post 0x{f['post']:x} != round sentinel 0x{want_post:x} (stale or wrong)")
    # ── target obligation from recorded residency ──
    obs_bit = 1 << obs_cpu
    in_gap = ge < sel < gr
    resident = bool(f["own_resident"] & obs_bit)
    targeted = bool(f["own_target"] & obs_bit)
    if in_gap and not resident:
        bad.append(f"{tag} residency 0x{f['own_resident']:x} omits the observer CPU {obs_cpu} "
                   f"while it is in its gap (inconsistent record)")
    if (in_gap or resident) and not targeted:
        bad.append(f"{tag} owner target 0x{f['own_target']:x} omits the observer CPU {obs_cpu} "
                   f"({'inside its gap' if in_gap else 'still resident'})")
    return bad


def grade(text, params):
    """Return (ok, lines). `params` must come from read_params (the pinned contract holds)."""
    out, fail = [], []
    by_round = collect_fields(text)
    extra = sorted(set(by_round) - set(range(1, CONTRACT_ROUNDS + 1)))
    if extra:
        fail.append(f"records for unscheduled rounds {extra}")
    recs = {}
    for r in range(1, CONTRACT_ROUNDS + 1):
        fields = by_round.get(r, {})
        rec, bad = {}, False
        for k in REQUIRED_FIELDS:
            vals = fields.get(k)
            if not vals:
                fail.append(f"round {r}: field '{k}' missing (missing/partial record)")
                bad = True
            elif len(vals) > 1:
                fail.append(f"round {r}: field '{k}' conflicting values {sorted(vals)}")
                bad = True
            else:
                rec[k] = next(iter(vals))
        if not bad:
            recs[r] = rec
    if fail:
        return False, out + fail

    # One observer incarnation across every round, from the gap owner's records.
    tids = {recs[r]["obs_tid"] for r in recs}
    observer_tid = next(iter(tids)) if len(tids) == 1 else None
    if observer_tid is None:
        fail.append(f"observer identity inconsistent across rounds: tids {sorted(tids)}")
    stamps = [recs[r][k] for r in recs for k in STAMPS]
    if len(set(stamps)) != len(stamps):
        fail.append("sequence stamps are not unique across the records (substituted record)")

    valid = []
    for r in range(1, CONTRACT_ROUNDS + 1):
        bad = validate_round(r, recs[r], params, observer_tid, recs.get(r - 1))
        if bad:
            fail.extend(bad)
        else:
            valid.append(r)

    credited = 0
    for r in valid:  # coverage is decided ONLY for valid rounds
        f = recs[r]
        ge, gr, sel, comp = f["gap_enter"], f["gap_release"], f["own_select"], f["own_complete"]
        if ge < sel < gr:
            credited += 1
            out.append(f"round {r}: valid, gap-credited (gap [{ge},{gr}] select {sel} complete {comp} "
                       f"post {f['post_seq']} observer cpu {f['obs_cpu']} target 0x{f['own_target']:x})")
        else:
            why = ("observer CPU had left A" if not f["own_resident"] & (1 << f["obs_cpu"])
                   else "observer CPU resident and targeted")
            out.append(f"round {r}: valid, completed but uncredited: select {sel} outside gap "
                       f"[{ge},{gr}] ({why})")

    out.append(f"completed_rounds={len(valid)}/{CONTRACT_ROUNDS} gap_credited={credited} "
               f"required={CONTRACT_GAP_THRESHOLD}")
    if len(valid) != CONTRACT_ROUNDS:
        fail.append(f"only {len(valid)}/{CONTRACT_ROUNDS} rounds are valid")
    if credited < CONTRACT_GAP_THRESHOLD:
        fail.append(f"gap-credited rounds {credited} < required {CONTRACT_GAP_THRESHOLD}")
    return not fail, out + fail


def evaluate(text):
    """The whole verdict for one log, exactly as the CLI computes it. Returns (ok, lines)."""
    if "VMI2_DUMP_END" not in text:
        return False, ["no VMI2_DUMP_END"]
    params, errors = read_params(text)
    if params is None:
        return False, errors
    return grade(text, params)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    if not args.log:
        ap.error("a log path or --self-test is required")
    with open(args.log, encoding="latin1") as f:
        text = f.read()
    ok, lines = evaluate(text)
    for line in lines:
        print("  " + line)
    print(f"VMI2_GRADE result={'ok' if ok else 'fail'} contract_rounds={CONTRACT_ROUNDS} "
          f"required_gaps={CONTRACT_GAP_THRESHOLD}")
    return 0 if ok else 1


# ───────────────────────────── self-test fixtures ─────────────────────────────
# Fixtures are built programmatically so every CRC is valid; a rejected fixture is therefore
# rejected for its record CONTENT, not for transport damage. Each case is graded twice: through
# evaluate() and through the real CLI entry point in a fresh interpreter; both must agree.

SEED = 0x5644493253454544
NEWBASE = 0x5644493249320000
OBS = 0x20080000
ASID = 4
BOOT_TID = 9701
OBS_TID = 9702


def field_lines(r, rec, copies=2):
    lines = []
    for k in REQUIRED_FIELDS:
        payload = f"r={r} k={k} v={rec.get(k, 0)}"
        lines.extend([f"VMI2_F {payload} crc={crc32(payload):08x}"] * copies)
    return lines


def healthy_round(r, observer_cpu=1, requester_cpu=0):
    """A valid, gap-credited round on a timeline the live witness actually produces: the gap opens,
    the owner selects inside it, the gap releases, the invalidation completes, the observer reads."""
    s = r * 10
    return {
        "declared": 1, "pre": SEED if r == 1 else NEWBASE + r - 1,
        "gap_enter": s, "own_select": s + 1, "gap_release": s + 2, "own_complete": s + 3,
        "post_seq": s + 4, "diag_target": (1 << observer_cpu) | (1 << requester_cpu),
        "obs_cpu": observer_cpu, "obs_tid": OBS_TID, "obs_asid": ASID,
        "own_cpu": requester_cpu, "own_asid": ASID, "own_va": OBS,
        "own_old": 0x10000 + r, "own_new": 0x20000 + r, "own_result": RES_REPLACED,
        "own_target": 1 << observer_cpu,
        "own_resident": (1 << observer_cpu) | (1 << requester_cpu),
        "own_acked": 1, "own_disp": DISP_SETTLED, "own_writes": 1, "refused": 0,
        "post": NEWBASE + r, "post_cpu": observer_cpu, "post_tid": OBS_TID, "post_asid": ASID,
    }


def before_gap(rec):
    """Valid but uncredited: the owner selected before the observer entered its drain (the live
    shape: the observer is still resident, so it is targeted)."""
    s = rec["gap_enter"]
    rec.update(own_select=s, gap_enter=s + 1, gap_release=s + 2, own_complete=s + 3, post_seq=s + 4)
    return rec


def after_release_departed(rec):
    """Valid but uncredited: the owner selected after the gap released, by which time the observer
    CPU had left A (its residency record cleared behind the idle-root flush) — no target owed."""
    s = rec["gap_enter"]
    rec.update(gap_release=s + 1, own_select=s + 2, own_complete=s + 3, post_seq=s + 4)
    rec["own_resident"] &= ~(1 << rec["obs_cpu"])
    rec["own_target"] = 0
    return rec


def wrap(records, prov=None, seal=None, extra_lines=None):
    prov = prov if prov is not None else (
        f"VMI2_WITNESS_PROVISIONED asid={ASID} boot_tid={BOOT_TID} rounds=8 gap_threshold=4 "
        f"obs_va=0x{OBS:x} mbx_phys=0x10321000 image=400 seed=0x{SEED:x} newval_base=0x{NEWBASE:x}")
    seal = seal if seal is not None else (
        f"VMI2_WITNESS_SEAL arch=x86_64 rounds=8 gap_threshold=4 seed=0x{SEED:x} "
        f"newval_base=0x{NEWBASE:x} obs_va=0x{OBS:x}")
    lines = [prov, "VMI2_GAP round=1 cpu=1 asid=4 current=empty root=loaded gap_enter=10 pre=0 diag_target=0x3"]
    for i, rec in enumerate(records):
        lines += field_lines(i + 1, rec)
    lines += extra_lines or []
    lines += [seal, "VMI2_DUMP_END"]
    return "\n".join(lines) + "\n"


def healthy(n=8, **kw):
    return [healthy_round(r, **kw) for r in range(1, n + 1)]


def self_test_cases():
    cases = []
    add = lambda name, log, want: cases.append((name, log, want))  # noqa: E731
    prov_line = (f"VMI2_WITNESS_PROVISIONED asid={ASID} boot_tid={BOOT_TID} rounds=8 gap_threshold=4 "
                 f"obs_va=0x{OBS:x} mbx_phys=0x10321000 image=400 seed=0x{SEED:x} newval_base=0x{NEWBASE:x}")
    seal_line = (f"VMI2_WITNESS_SEAL arch=x86_64 rounds=8 gap_threshold=4 seed=0x{SEED:x} "
                 f"newval_base=0x{NEWBASE:x} obs_va=0x{OBS:x}")

    # ── positive ──
    add("healthy", wrap(healthy()), True)
    add("healthy_smp4_observer_cpu3", wrap(healthy(observer_cpu=3, requester_cpu=0)), True)
    add("extra_redundant_copy", wrap(healthy(), extra_lines=field_lines(1, healthy_round(1), copies=1)), True)
    add("torn_copy_tolerated", wrap(healthy(), extra_lines=["VMI2_F r=2 k=post v=999 crc=deadbeef"]), True)
    add("redundant_declaration_copies", wrap(healthy(), extra_lines=[prov_line, seal_line]), True)
    recs = healthy()
    for r in (5, 6, 7, 8):
        before_gap(recs[r - 1])
    add("uncredited_before_gap_still_meets_contract", wrap(recs), True)
    recs = healthy()
    for r in (2, 4):
        before_gap(recs[r - 1])
    for r in (6, 8):
        after_release_departed(recs[r - 1])
    add("uncredited_interference_and_departure", wrap(recs), True)

    # ── step-1 reproductions (each passed the delivered grader) ──
    recs = healthy(observer_cpu=1, requester_cpu=0)
    for rec in recs:
        rec["own_target"] = 1 << 2  # a valid third CPU on a 4-CPU topology, not the observer
    add("A_wrong_third_cpu_every_round", wrap(recs), False)
    recs = healthy()
    recs[2]["own_target"] = 1 << 2
    add("A_wrong_third_cpu_one_round", wrap(recs), False)
    recs = healthy()
    before_gap(recs[4])
    recs[4]["own_complete"], recs[4]["own_select"] = recs[4]["own_select"], recs[4]["own_complete"]
    add("B1_uncredited_round_complete_before_select", wrap(recs), False)
    recs = healthy()
    before_gap(recs[4])
    recs[4]["own_complete"], recs[4]["post_seq"] = recs[4]["post_seq"], recs[4]["own_complete"]
    add("B2_uncredited_round_post_before_complete", wrap(recs), False)
    recs = healthy()
    for rec in recs:
        before_gap(rec)
    add("C_all_outside_seal_threshold_0", wrap(recs, seal=seal_line.replace("gap_threshold=4", "gap_threshold=0")), False)
    add("C_contract_lowered_in_both", wrap(recs, prov=prov_line.replace("gap_threshold=4", "gap_threshold=0"),
                                          seal=seal_line.replace("gap_threshold=4", "gap_threshold=0")), False)
    add("C_all_outside_contract_intact", wrap(recs), False)
    recs = healthy()
    for r in (5, 6, 7, 8):  # the delivered fixture: selection past release, completion/post before it
        recs[r - 1]["own_select"] = recs[r - 1]["gap_release"] + 5
    add("D_delivered_some_uncredited_timeline", wrap(recs), False)

    # ── contract / declaration ──
    add("seal_missing", wrap(healthy(), seal=""), False)
    add("provisioning_missing", wrap(healthy(), prov=""), False)
    add("seal_conflicting_copy", wrap(healthy(), extra_lines=[seal_line.replace("seed=0x", "seed=0x1")]), False)
    add("rounds_declared_7", wrap(healthy(), seal=seal_line.replace("rounds=8", "rounds=7")), False)
    add("torn_seal_fragment_lowers_threshold", wrap(healthy(), extra_lines=["VMI2_WITNESS_SEAL arch=x86_64 rounds=8 gap_threshold=0"]), False)
    add("summary_without_records", wrap([]), False)
    add("only_one_round", wrap(healthy(1)), False)
    add("counters_without_records", wrap(healthy(3)), False)
    add("unscheduled_round_9", wrap(healthy(), extra_lines=field_lines(9, healthy_round(8))), False)

    # ── observer identity ──
    recs = healthy()
    recs[3]["obs_tid"] = OBS_TID + 1
    add("observer_tid_inconsistent", wrap(recs), False)
    recs = healthy()
    for rec in recs:
        rec["obs_tid"] = rec["post_tid"] = 0
    add("observer_identity_missing", wrap(recs), False)
    recs = healthy()
    for rec in recs:
        rec["obs_tid"] = rec["post_tid"] = BOOT_TID
    add("observer_is_boot_task", wrap(recs), False)
    recs = healthy()
    recs[1]["post_tid"] = OBS_TID + 7
    add("post_producer_not_observer", wrap(recs), False)
    recs = healthy()
    recs[1]["obs_asid"] = ASID + 1
    add("observer_asid_wrong", wrap(recs), False)
    recs = healthy(observer_cpu=0, requester_cpu=0)
    for rec in recs:
        rec["own_target"] = 1 << 1
    add("observer_cpu_is_requester", wrap(recs), False)

    # ── target obligation / residency ──
    recs = healthy()
    recs[4]["own_target"] = 0
    add("placement_only_target_in_gap", wrap(recs), False)
    recs = healthy()
    recs[4]["own_resident"] = 1 << 0
    add("residency_omits_observer_in_gap", wrap(recs), False)
    recs = healthy()
    before_gap(recs[4])
    recs[4]["own_target"] = 0  # still resident, outside the gap, untargeted
    add("resident_outside_gap_untargeted", wrap(recs), False)
    recs = healthy()
    recs[4]["own_target"] |= 1 << 0
    add("owner_target_names_requester", wrap(recs), False)

    # ── replacement, completion, values, ordering, transport ──
    recs = healthy()
    recs[3]["refused"] = 1
    add("refused_replacement", wrap(recs), False)
    recs = healthy()
    recs[2]["own_result"], recs[2]["own_old"] = RES_NO_DISPLACE, 0
    add("no_displacement", wrap(recs), False)
    recs = healthy()
    recs[2]["own_new"] = recs[2]["own_old"]
    add("same_backing", wrap(recs), False)
    recs = healthy()
    recs[2]["own_writes"] = 2
    add("double_owner_write", wrap(recs), False)
    recs = healthy()
    recs[5]["post"] = NEWBASE + 4
    add("stale_post", wrap(recs), False)
    recs = healthy()
    recs[5]["pre"] = NEWBASE + 1
    add("wrong_pre", wrap(recs), False)
    recs = healthy()
    recs[1]["own_acked"] = 0
    add("not_acknowledged", wrap(recs), False)
    recs = healthy()
    recs[0]["own_acked"], recs[0]["own_disp"] = 0, DISP_SETTLED
    add("premature_reclaim", wrap(recs), False)
    recs = healthy()
    recs[0]["own_disp"] = DISP_QUARANTINED
    add("quarantined_after_ack", wrap(recs), False)
    recs = healthy()  # the delivered healthy fixture's shape: select < complete < post, all inside the gap
    s = recs[2]["gap_enter"]
    recs[2].update(own_select=s + 1, own_complete=s + 2, post_seq=s + 3, gap_release=s + 5)
    add("post_read_inside_gap", wrap(recs), False)
    recs = healthy()
    recs[4]["gap_enter"] = recs[2]["post_seq"] + 5  # unused stamp 39, before round 4's post-read 44
    add("cross_round_order_violated", wrap(recs), False)
    recs = healthy()
    recs[6]["own_complete"] = recs[5]["own_complete"]
    add("duplicate_stamp", wrap(recs), False)
    recs = healthy()
    recs[0]["own_asid"] = ASID + 1
    add("wrong_owner_asid", wrap(recs), False)
    add("conflicting_records", wrap(healthy(), extra_lines=[
        f"VMI2_F r=7 k=own_new v=39321 crc={crc32('r=7 k=own_new v=39321'):08x}"]), False)
    recs = healthy()
    recs[1]["own_va"] = OBS + 0x1000
    add("wrong_address", wrap(recs), False)
    return cases


def self_test():
    cases = self_test_cases()
    npass = 0
    here = os.path.abspath(__file__)
    with tempfile.TemporaryDirectory() as tmp:
        for name, log, want in cases:
            ok_fn, _ = evaluate(log)
            path = os.path.join(tmp, name + ".log")
            with open(path, "w", encoding="latin1") as f:
                f.write(log)
            rc = subprocess.run([sys.executable, "-I", here, path],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
            ok_cli = rc == 0
            good = ok_fn == want and ok_cli == want
            npass += good
            print(f"  self-test {name}: evaluate={'pass' if ok_fn else 'fail'} "
                  f"cli={'pass' if ok_cli else 'fail'} want={'pass' if want else 'fail'} "
                  f"[{'ok' if good else 'MISMATCH'}]")
    total = len(cases)
    print(f"VMI2_GRADER_SELF_TEST cases={total} passed={npass} result={'ok' if npass == total else 'fail'}")
    return 0 if npass == total else 1


if __name__ == "__main__":
    sys.exit(main())
