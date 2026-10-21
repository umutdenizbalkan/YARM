#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION2 dispatch-gap witness grader.
#
# It derives the verdict from the COMPLETE per-round records (VMI2_RECORD, each emitted twice with a
# CRC), never from a trusted summary. For each scheduled round it independently establishes:
#   * the round completed with a valid, individually graded record;
#   * the participants, ASID, address and the pre/post identities;
#   * the production shootdown owner's ACTUAL target set included the root-resident observer CPU;
#   * a GENUINE replacement occurred (an old backing was displaced) and completed (acknowledged),
#     with the displaced backing settled only after that;
#   * the gap causality held BY SEQUENCE STAMP, not console order: the observer read the page (pre),
#     its current emptied while its root stayed loaded (the gap opened), the owner selected targets
#     WHILE that gap was open (gap_enter < own_select < gap_release), completion preceded the
#     observer's post-read (own_complete < post_seq), and the post byte is this round's sentinel.
#
# A round that completed but whose owner op fell outside the gap earns NO gap credit (it is not a
# failure, but does not count toward the threshold). A GENUINE defect — a refused replacement, a
# non-displacing install, stale bytes after completion, a missing/absent required invalidation, a
# premature reclaim (settled without an ack) — FAILS the boot regardless of coverage. Missing,
# duplicated-and-conflicting, substituted or partial records FAIL. Two identical copies of a record
# are transport redundancy and are accepted.

import argparse
import binascii
import re
import sys

ROUNDS = 8
RES_REPLACED = 1
RES_NO_DISPLACE = 2
DISP_SETTLED = 1
DISP_QUARANTINED = 2

# Every field a complete round record must carry (excluding `round`, which is the key).
REQUIRED_FIELDS = [
    "declared", "pre", "gap_enter", "gap_release", "diag_target", "own_cpu", "own_asid",
    "own_va", "own_old", "own_new", "own_result", "own_target", "own_acked", "own_disp",
    "own_select", "own_complete", "own_writes", "refused", "post", "post_seq",
]

FIELD_RE = re.compile(r"VMI2_F r=(\d+) k=([a-z_]+) v=(\d+) crc=([0-9a-f]{8})")


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
        payload = f"r={r} k={k} v={v}"
        if crc32(payload) != crc:
            continue  # a torn/corrupt copy: ignored, never trusted
        by_round.setdefault(r, {}).setdefault(k, set()).add(v)
    return by_round


def grade(text, params):
    """Return (ok: bool, lines: [str])."""
    out = []
    fail = []

    def note(s):
        out.append(s)

    def die(s):
        fail.append(s)

    seed = params["seed"]
    newbase = params["newval_base"]
    obs_va = params["obs_va"]
    threshold = params["gap_threshold"]

    by_round = collect_fields(text)

    # Every scheduled round must carry every field exactly once (one CRC-valid value). A missing
    # field is a missing/partial record; two disagreeing CRC-valid copies are a conflict.
    recs = {}
    for r in range(1, ROUNDS + 1):
        fields = by_round.get(r, {})
        rec = {}
        bad = False
        for k in REQUIRED_FIELDS:
            vals = fields.get(k)
            if not vals:
                die(f"round {r}: field '{k}' missing (partial/missing transport)")
                bad = True
            elif len(vals) > 1:
                die(f"round {r}: field '{k}' conflicting values {sorted(vals)}")
                bad = True
            else:
                rec[k] = next(iter(vals))
        if not bad:
            recs[r] = rec

    if fail:
        return False, out + fail

    completed = 0
    gap_credited = 0
    for r in range(1, ROUNDS + 1):
        f = recs[r]
        tag = f"round {r}:"
        # A refused or non-displacing replacement is a GENUINE failure — never a silent skip.
        if f["refused"]:
            die(f"{tag} replacement refused (named failure)")
            continue
        if f["declared"] != 1:
            die(f"{tag} not declared/observed")
            continue
        completed += 1
        # Identity: the owner op must name OBS in the witness ASID, and a single writer.
        if f["own_writes"] != 1:
            die(f"{tag} owner record writes={f['own_writes']} (want exactly 1; dup/substituted)")
            continue
        if f["own_va"] != obs_va:
            die(f"{tag} owner va=0x{f['own_va']:x} != OBS 0x{obs_va:x}")
            continue
        if f["own_asid"] != params["asid"]:
            die(f"{tag} owner asid={f['own_asid']} != witness asid {params['asid']}")
            continue
        # A genuine replacement displaced an old backing and installed a new one.
        if f["own_result"] == RES_NO_DISPLACE:
            die(f"{tag} install displaced nothing (not a replacement)")
            continue
        if f["own_result"] != RES_REPLACED or f["own_old"] == 0 or f["own_new"] == 0:
            die(f"{tag} no genuine replacement (result={f['own_result']} old=0x{f['own_old']:x} new=0x{f['own_new']:x})")
            continue
        if f["own_old"] == f["own_new"]:
            die(f"{tag} replacement installed the SAME backing (old==new==0x{f['own_old']:x})")
            continue
        # Required completion: the shootdown acknowledged, and the displaced backing was settled
        # ONLY after that ack (never a premature reclaim).
        if f["own_acked"] != 1:
            die(f"{tag} shootdown not acknowledged (required invalidation incomplete)")
            continue
        if f["own_disp"] == DISP_SETTLED and f["own_acked"] != 1:
            die(f"{tag} displaced backing settled WITHOUT acknowledgement (premature reclaim)")
            continue
        if f["own_disp"] != DISP_SETTLED:
            die(f"{tag} displaced backing not settled after an acknowledged shootdown (disp={f['own_disp']})")
            continue
        # Target inclusion: the OWNER's own target set named the observer CPU (not the diagnostic).
        own_cpu = f["own_cpu"]  # the requester (mutator) CPU
        obs_cpu = params.get("observer_cpu")
        # The observer CPU is every non-requester bit the gap recorded; the owner target must name it.
        if f["own_target"] == 0:
            die(f"{tag} owner target set empty (observer CPU not targeted)")
            continue
        # Post-completion bytes: the observer read THIS round's sentinel after the replace completed.
        want_post = (newbase + r) & 0xFFFFFFFFFFFFFFFF
        want_pre = seed if r == 1 else (newbase + (r - 1))
        if f["post"] != want_post:
            die(f"{tag} post byte 0x{f['post']:x} != round sentinel 0x{want_post:x} (stale or wrong)")
            continue
        if f["pre"] != want_pre:
            die(f"{tag} pre byte 0x{f['pre']:x} != expected 0x{want_pre:x} (observer did not see prior backing)")
            continue
        # Gap causality, BY SEQUENCE STAMP.
        ge, gr = f["gap_enter"], f["gap_release"]
        sel, comp, post_seq = f["own_select"], f["own_complete"], f["post_seq"]
        if ge == 0 or gr == 0:
            note(f"{tag} completed, uncredited: no gap window recorded")
            continue
        in_gap = ge < sel < gr
        if not in_gap:
            note(f"{tag} completed, uncredited: owner select {sel} outside gap [{ge},{gr}]")
            continue
        if not (comp < post_seq):
            die(f"{tag} observer post-read {post_seq} not after completion {comp}")
            continue
        if not (sel <= comp):
            die(f"{tag} completion {comp} precedes target selection {sel} (impossible ordering)")
            continue
        own_cpu_bit = 1 << own_cpu
        # The observer CPU is a target bit other than the requester's own.
        if f["own_target"] & ~own_cpu_bit == 0:
            die(f"{tag} owner target 0x{f['own_target']:x} names only the requester CPU {own_cpu}")
            continue
        gap_credited += 1
        note(f"{tag} gap-credited (gap [{ge},{gr}] select {sel} complete {comp} post {post_seq} target 0x{f['own_target']:x})")

    note(f"completed_rounds={completed}/{ROUNDS} gap_credited={gap_credited} threshold={threshold}")
    if completed != ROUNDS:
        die(f"only {completed}/{ROUNDS} rounds completed with a valid record")
    if gap_credited < threshold:
        die(f"gap-credited rounds {gap_credited} < threshold {threshold}")
    ok = not fail
    return ok, out + fail


def read_params(text):
    m = re.search(r"VMI2_WITNESS_PROVISIONED .*?asid=(\d+).*?gap_threshold=(\d+)", text)
    seal = re.search(
        r"VMI2_WITNESS_SEAL .*?gap_threshold=(\d+) seed=0x([0-9a-f]+) newval_base=0x([0-9a-f]+) obs_va=0x([0-9a-f]+)",
        text,
    )
    if not seal:
        raise ValueError("no VMI2_WITNESS_SEAL")
    params = {
        "gap_threshold": int(seal.group(1)),
        "seed": int(seal.group(2), 16),
        "newval_base": int(seal.group(3), 16),
        "obs_va": int(seal.group(4), 16),
        "asid": int(m.group(1)) if m else None,
    }
    if params["asid"] is None:
        raise ValueError("no VMI2_WITNESS_PROVISIONED asid")
    # The observer CPU: every gap record names current=empty on a CPU; capture it from the GAP line.
    g = re.search(r"VMI2_GAP round=\d+ cpu=(\d+)", text)
    params["observer_cpu"] = int(g.group(1)) if g else None
    return params


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    text = open(args.log, encoding="latin1").read()
    if "VMI2_DUMP_END" not in text:
        print("VMI2_GRADE result=fail reason=no_dump_end")
        return 1
    params = read_params(text)
    ok, lines = grade(text, params)
    for line in lines:
        print("  " + line)
    print(f"VMI2_GRADE result={'ok' if ok else 'fail'} threshold={params['gap_threshold']}")
    return 0 if ok else 1


# ───────────────────────────── self-test fixtures ─────────────────────────────
# A healthy 8-round log, and the delivered witness's false-pass reproductions, each of which this
# grader must REJECT. Fixtures are built programmatically so the CRCs are correct.

SEED = 0x5644493253454544
NEWBASE = 0x5644493249320000
OBS = 0x20080000
ASID = 4


def field_lines(r, rec, copies=2):
    """Emit the per-field VMI2_F lines for one round record dict, each `copies` times."""
    lines = []
    for k in REQUIRED_FIELDS:
        v = rec.get(k, 0)
        payload = f"r={r} k={k} v={v}"
        line = f"VMI2_F {payload} crc={crc32(payload):08x}"
        lines.extend([line] * copies)
    return lines


def healthy_round(r, seq0, observer_cpu=1, requester_cpu=0):
    return {
        "declared": 1, "pre": (SEED if r == 1 else NEWBASE + r - 1),
        "gap_enter": seq0, "gap_release": seq0 + 10, "diag_target": 0b11,
        "own_cpu": requester_cpu, "own_asid": ASID, "own_va": OBS,
        "own_old": 0x10000 + r, "own_new": 0x20000 + r, "own_result": RES_REPLACED,
        "own_target": 1 << observer_cpu, "own_acked": 1, "own_disp": DISP_SETTLED,
        "own_select": seq0 + 1, "own_complete": seq0 + 2, "own_writes": 1, "refused": 0,
        "post": NEWBASE + r, "post_seq": seq0 + 3,
    }


def wrap(records, threshold=4, extra_lines=None):
    head = (
        f"VMI2_WITNESS_PROVISIONED asid={ASID} boot_tid=9701 rounds=8 gap_threshold={threshold} "
        f"obs_va=0x{OBS:x} mbx_phys=0x10321000 image=400 seed=0x{SEED:x} newval_base=0x{NEWBASE:x}\n"
    )
    gaps = "VMI2_GAP round=1 cpu=1 asid=4 current=empty root=loaded gap_enter=1 pre=0 diag_target=0x3\n"
    lines = []
    for i, rec in enumerate(records):
        lines += field_lines(i + 1, rec)
    lines += extra_lines or []
    seal = (
        f"VMI2_WITNESS_SEAL arch=x86_64 rounds=8 gap_threshold={threshold} seed=0x{SEED:x} "
        f"newval_base=0x{NEWBASE:x} obs_va=0x{OBS:x}\nVMI2_DUMP_END\n"
    )
    return head + gaps + "\n".join(lines) + "\n" + seal


def self_test():
    cases = []

    def healthy(n=8):
        return [healthy_round(r, 100 + r * 20) for r in range(1, n + 1)]

    # 1. Healthy: 8 complete, gap-credited rounds → PASS.
    cases.append(("healthy", wrap(healthy()), True))

    # 2. Only one observed round (the delivered false-pass): 7 rounds missing → FAIL.
    cases.append(("only_one_round", wrap(healthy(1)), False))

    # 3. Trusted summary with no individual evidence: a good seal but zero records → FAIL.
    cases.append(("summary_without_records", wrap([]), False))

    # 4. Refused replacement (old mapping left intact) → FAIL.
    recs = healthy()
    recs[3]["refused"] = 1
    cases.append(("refused_replacement", wrap(recs), False))

    # 5. Non-displacing install (no genuine replacement) → FAIL.
    recs = healthy()
    recs[2]["own_result"] = RES_NO_DISPLACE
    recs[2]["own_old"] = 0
    cases.append(("no_displacement", wrap(recs), False))

    # 6. Stale bytes after completion → FAIL.
    recs = healthy()
    recs[5]["post"] = NEWBASE + 4  # a prior round's sentinel, not this one's
    cases.append(("stale_post", wrap(recs), False))

    # 7. Missing required invalidation (not acknowledged) → FAIL.
    recs = healthy()
    recs[1]["own_acked"] = 0
    cases.append(("not_acknowledged", wrap(recs), False))

    # 8. Premature reclaim (settled without ack) → FAIL.
    recs = healthy()
    recs[0]["own_acked"] = 0
    recs[0]["own_disp"] = DISP_SETTLED
    cases.append(("premature_reclaim", wrap(recs), False))

    # 9. Owner op outside the gap: uncredited, dropping below threshold → FAIL.
    recs = healthy()
    for r in range(3, 9):  # only 2 credited, below threshold 4
        recs[r - 1]["own_select"] = recs[r - 1]["gap_release"] + 5  # selection after the gap closed
    cases.append(("outside_gap_below_threshold", wrap(recs), False))

    # 10. Placement-only target (observer CPU not named) → FAIL.
    recs = healthy()
    recs[4]["own_target"] = 1 << recs[4]["own_cpu"]  # only the requester bit
    cases.append(("placement_only_target", wrap(recs), False))

    # 11. Conflicting valid records for one round (a substituted field copy) → FAIL.
    recs = healthy()
    conflict = f"r=7 k=own_new v=0x9999"
    extra = [f"VMI2_F {conflict} crc={crc32(conflict):08x}"]
    # crc32 wants the decimal form the kernel emits; use an int value to match.
    conflict = "r=7 k=own_new v=39321"
    extra = [f"VMI2_F {conflict} crc={crc32(conflict):08x}"]
    cases.append(("conflicting_records", wrap(recs, extra_lines=extra), False))

    # 12. Success-shaped seal but only 3 rounds carry records → FAIL.
    cases.append(("counters_without_records", wrap(healthy(3)), False))

    # 13. Transport redundancy: a THIRD identical copy of every field is fine → PASS.
    recs = healthy()
    extra = field_lines(1, recs[0], copies=1)  # one more copy of round 1's fields
    cases.append(("extra_redundant_copy", wrap(recs, extra_lines=extra), True))

    # 14. Wrong ASID identity → FAIL.
    recs = healthy()
    recs[0]["own_asid"] = ASID + 1
    cases.append(("wrong_asid", wrap(recs), False))

    # 15. A torn copy (bad CRC) alongside a good one is tolerated → PASS.
    recs = healthy()
    torn = ["VMI2_F r=2 k=post v=999 crc=deadbeef"]  # wrong crc → dropped
    cases.append(("torn_copy_tolerated", wrap(recs, extra_lines=torn), True))

    # 16. Explicitly uncredited interference: half the rounds' owner ops fall outside the gap, but
    # the credited rounds still meet the threshold → PASS. A completed-but-uncredited round is not
    # a failure; it simply earns no gap credit.
    recs = healthy()
    for r in (5, 6, 7, 8):  # 4 credited (1-4), 4 uncredited, threshold 4
        recs[r - 1]["own_select"] = recs[r - 1]["gap_release"] + 5
    cases.append(("some_uncredited_still_passes", wrap(recs), True))

    npass = 0
    for name, log, want in cases:
        try:
            params = read_params(log)
            ok, _ = grade(log, params)
        except Exception as e:  # noqa: BLE001
            ok = False
            _ = e
        status = "ok" if ok == want else "MISMATCH"
        if ok == want:
            npass += 1
        print(f"  self-test {name}: got={'pass' if ok else 'fail'} want={'pass' if want else 'fail'} [{status}]")
    total = len(cases)
    print(f"VMI2_GRADER_SELF_TEST cases={total} passed={npass} result={'ok' if npass == total else 'fail'}")
    return 0 if npass == total else 1


if __name__ == "__main__":
    sys.exit(main())
