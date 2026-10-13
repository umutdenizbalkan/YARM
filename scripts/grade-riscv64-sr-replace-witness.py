#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""Grade the RISC-V shared-region replacement witness (riscv64-sr-replace-witness).

Usage:
  grade-riscv64-sr-replace-witness.py <boot.norm.log>   grade one boot
  grade-riscv64-sr-replace-witness.py --self-test       run the fixture controls

AP-SCHED1 replaced the SHARED-REGION-LIFETIME2 grader's console-position comparisons. Two harts
print to one console: a line's position says only when its print happened, and the install line is
printed after the install's outcome is known, i.e. after the remote hart's fence completed. So
the observer may legitimately print its observation first.

Every ordering obligation is checked on the witness's `seq=` stamps instead: one witness-only
atomic counter, drawn at each event (target set, fence request, fence completion, install outcome,
settlement, each user step). The obligations, each checked independently of the kernel's own
verdict line:

  identity     every record names the provisioned tasks, address space, window, objects and
               physical pages, and the fence's hart mask is exactly CPU 1's real hart;
  residency    O primed W on CPU 1 and took no supervisor entry until its observation, and its
               residency began before the replacement's invalidation was planned;
  invalidation the replacement's own target set (requester CPU 0) includes CPU 1 and excludes the
               requester; the fence was requested for that hart after the target set and completed
               successfully before the outcome; the outcome is complete;
  bytes        R read the granted bytes after the outcome; O read the granted byte, with a stamp
               after the fence request it depends on;
  settlement   the displaced hold settled once, after the outcome, reclaiming exactly the
               displaced object and page.
"""
import re
import sys

R_TID, O_TID = "9400", "9401"
NEW_BYTE, OLD_BYTE = 0x5A, 0x11
W_VA = 0x20080000

FORBIDDEN = [
    "SHARED_REGION_MAP_INVALIDATION_INCOMPLETE", "VM_DISPLACED_HOLD_RETAINED",
    "VM_MAP_DISPLACEMENT_REFUSED", "KERNEL PANIC", "panicked at", "SRR_WITNESS_PLACEMENT_FAIL",
    "step=O_STALE", "step=R_RECV_FAIL", "step=R_SEND_FAIL", "step=R_BYTES_FAIL",
    "step=R_NO_OBSERVER", "step=O_PRIME_FAIL",
]


def fields(line):
    return dict(re.findall(r"(\w+)=(\S+)", line))


def num(v):
    return int(v, 0)


def grade(lines):
    fails = []

    def one(pat, why):
        hits = [l for l in lines if re.search(pat, l)]
        if len(hits) != 1:
            fails.append(f"{why} (found {len(hits)})")
            return None
        return fields(hits[0])

    prov = one(r"SRR_WITNESS_PROVISIONED ", "provisioned once")
    placed = one(r"SRR_WITNESS_SECONDARY_PLACED ", "observer placed once")
    o_ready = one(r"SRR_USER .*step=O_READY ", "O primed once")
    o_obs = one(r"SRR_USER .*step=O_(NEW|STALE) ", "O reported one observation")
    r_ok = one(r"SRR_USER .*step=R_OK ", "R reported once")
    inst = one(r"SRR_KERNEL_INSTALL ", "one displacing install at W")
    order = one(r"SRR_KERNEL_INSTALL_ORDER ", "the install's causal stamps once")
    if inst and order:
        inst = {**inst, **order}
    settle = one(r"SRR_KERNEL_SETTLE ", "the displaced hold settled once")
    # The kernel's own verdict must agree; it is never the proof on its own.
    one(r"SRR_VERDICT .*result=ok$", "the kernel's own verdict agrees")
    for bad in FORBIDDEN:
        if any(bad in l for l in lines):
            fails.append(f"forbidden: {bad}")
    if not all([prov, placed, o_ready, o_obs, r_ok, inst, order, settle]):
        return fails
    try:
        # ── identity ───────────────────────────────────────────────────────────────────────
        asid, old_phys, new_phys = prov["asid"], num(prov["old_phys"]), num(prov["new_phys"])
        old_obj = prov["old_object"]
        if prov["r_tid"] != R_TID or prov["o_tid"] != O_TID or num(prov["w_va"]) != W_VA:
            fails.append("identity: provisioned tasks / window")
        if placed["cpu"] != "1" or placed["tid"] != O_TID or placed["kick"] != "1":
            fails.append("identity: O placed on CPU 1 with its start-up kick")
        hart = int(placed["hart"])
        if hart < 0:
            fails.append("identity: CPU 1's hart unknown")
        for name, rec, cpu, tid in [("O_READY", o_ready, "1", O_TID), ("O observation", o_obs, "1", O_TID),
                                    ("R_OK", r_ok, "0", R_TID)]:
            if rec["cpu"] != cpu or rec["tid"] != tid:
                fails.append(f"identity: {name} is not task {tid} on CPU {cpu}")
        if inst["asid"] != asid or num(inst["va"]) != W_VA:
            fails.append("identity: the install is not at W in the provisioned space")
        if num(inst["old_phys"]) != old_phys:
            fails.append("identity: the install displaced a different page")
        if num(inst["new_phys"]) != new_phys:
            fails.append("identity: the install mapped a page other than the granted object's")
        if settle["object"] != old_obj or settle["expected_object"] != old_obj or num(settle["phys"]) != old_phys:
            fails.append("identity: the settlement names a different object or page")
        # ── invalidation ───────────────────────────────────────────────────────────────────
        targets = num(inst["targets"])
        if inst["requester_cpu"] != "0":
            fails.append("invalidation: the requester is not CPU 0")
        if targets & 0b10 == 0:
            fails.append("invalidation: CPU 1 is not in the replacement's target set")
        if targets & 0b01:
            fails.append("invalidation: the requester targeted itself")
        if inst["fence"] != "1":
            fails.append("invalidation: the remote fence did not succeed")
        if hart >= 0 and (num(inst["fence_req_mask"]) != 1 << hart or num(inst["fence_hart_mask"]) != 1 << hart):
            fails.append("invalidation: the fence's hart mask is not CPU 1's hart")
        if inst["outcome"] != "complete":
            fails.append("invalidation: the outcome is not complete")
        t, fr, fd, s_inst = (int(inst["targets_seq"]), int(inst["fence_req_seq"]),
                             int(inst["fence_done_seq"]), int(inst["seq"]))
        if not (0 < t < fr < fd < s_inst):
            fails.append(f"invalidation: causal order targets<fence_req<fence_done<outcome broken ({t},{fr},{fd},{s_inst})")
        # ── residency ──────────────────────────────────────────────────────────────────────
        if int(o_obs["entries"]) != int(o_ready["entries"]) + 1:
            fails.append("residency: O took a supervisor entry between priming and observing")
        if not int(o_ready["seq"]) < t:
            fails.append("residency: O primed only after the invalidation was planned")
        # ── bytes ──────────────────────────────────────────────────────────────────────────
        if o_obs["step"] != "O_NEW" or num(o_obs["aux"]) != NEW_BYTE:
            fails.append("bytes: O did not read the granted byte")
        if not int(o_obs["seq"]) > fr:
            fails.append("bytes: O's observation precedes the fence request it depends on")
        if num(r_ok["aux"]) != 0x1000 or not int(r_ok["seq"]) > s_inst:
            fails.append("bytes: R did not read the granted page after the outcome")
        # ── settlement ─────────────────────────────────────────────────────────────────────
        if settle["reclaimed"] != "1":
            fails.append("settlement: the displaced object was not reclaimed")
        if not int(settle["seq"]) > s_inst:
            fails.append("settlement: settled before the outcome was known")
    except (KeyError, ValueError) as e:
        fails.append(f"malformed record: {e!r}")
    return fails


# ─────────────────────────────── fixture controls ───────────────────────────────

def fixture():
    """A passing boot in the LEGITIMATE reversed print order: O's observation is printed before
    the requester's install line, while its stamp follows the fence request."""
    return [
        "SRR_WITNESS_PROVISIONED r_tid=9400 o_tid=9401 asid=4 w_va=0x20080000 old_object=1 old_phys=0x86161000 new_object=2 new_phys=0x86162000 r_image=448 o_image=272",
        "SRR_WITNESS_SECONDARY_PLACED cpu=1 tid=9401 kick=1 hart=0",
        "SRR_USER cpu=0 tid=9400 step=R_ENTERED round=0 aux=0x0 entries=1810 seq=1",
        "SRR_USER cpu=1 tid=9401 step=O_READY round=0 aux=0x0 entries=969 seq=2",
        "SRR_USER cpu=1 tid=9401 step=O_NEW round=0 aux=0x5a entries=970 seq=6",
        "SRR_KERNEL_INSTALL route=queued requester_cpu=0 asid=4 va=0x20080000 old_phys=0x86161000 new_phys=0x86162000 hold=1 targets=0x2 fence=1 fence_hart_mask=0x1 outcome=complete",
        "SRR_KERNEL_INSTALL_ORDER seq=7 targets_seq=3 fence_req_seq=4 fence_req_mask=0x1 fence_done_seq=5",
        "SRR_KERNEL_SETTLE object=1 expected_object=1 phys=0x86161000 reclaimed=1 seq=8",
        "SRR_USER cpu=0 tid=9400 step=R_OK round=0 aux=0x1000 entries=1824 seq=9",
        "SRR_VERDICT bytes=1 receiver=1 observer=1 observer_credited=1 kernel=1 requester=0 targets=0x2 fence=1 install=1 settle=1 result=ok",
    ]


def mutate(lines, pat, repl):
    out = [re.sub(pat, repl, l) for l in lines]
    assert out != lines, pat
    return out


def self_test():
    base = fixture()
    in_order = base[:4] + [base[5], base[6], base[4]] + base[7:]
    cases = [
        ("pass: reversed print order", base, True),
        ("pass: in print order", in_order, True),
        ("missing invalidation: no remote target", mutate(base, r"targets=0x2", "targets=0x0"), False),
        ("missing invalidation: no fence", mutate(base, r" fence=1 ", " fence=0 "), False),
        ("missing invalidation: fence failed", mutate(base, r" fence=1 ", " fence=2 "), False),
        ("missing invalidation: incomplete outcome", mutate(base, r"outcome=complete", "outcome=invalidation_incomplete"), False),
        ("wrong targets: the requester instead of CPU 1", mutate(base, r"targets=0x2", "targets=0x1"), False),
        ("wrong targets: wrong requester", mutate(base, r"requester_cpu=0", "requester_cpu=1"), False),
        ("wrong targets: fence to the wrong hart", mutate(base, r"fence_req_mask=0x1", "fence_req_mask=0x2"), False),
        ("stale bytes: O read the old byte", mutate(base, r"step=O_NEW round=0 aux=0x5a", "step=O_STALE round=0 aux=0x11"), False),
        ("stale bytes: O's byte is not the granted one", mutate(base, r"step=O_NEW round=0 aux=0x5a", "step=O_NEW round=0 aux=0x11"), False),
        ("stale bytes: R read a short page", mutate(base, r"step=R_OK round=0 aux=0x1000", "step=R_OK round=0 aux=0x800"), False),
        ("causal: O observed before the fence was requested", mutate(base, r"entries=970 seq=6", "entries=970 seq=3"), False),
        ("causal: fence completed after the outcome", mutate(base, r"fence_done_seq=5", "fence_done_seq=8"), False),
        ("residency: O entered the kernel before observing", mutate(base, r"entries=970", "entries=971"), False),
        ("residency: O primed after the plan", mutate(base, r"entries=969 seq=2", "entries=969 seq=4"), False),
        ("missing settlement", [l for l in base if "SRR_KERNEL_SETTLE" not in l], False),
        ("missing settlement: not reclaimed", mutate(base, r"reclaimed=1", "reclaimed=0"), False),
        ("missing settlement: settled before the outcome", mutate(base, r"reclaimed=1 seq=8", "reclaimed=1 seq=6"), False),
        ("substituted identity: address space", mutate(base, r"requester_cpu=0 asid=4", "requester_cpu=0 asid=5"), False),
        ("substituted identity: displaced page", mutate(base, r"va=0x20080000 old_phys=0x86161000", "va=0x20080000 old_phys=0x86171000"), False),
        ("substituted identity: granted page", mutate(base, r"old_phys=0x86161000 new_phys=0x86162000 hold", "old_phys=0x86161000 new_phys=0x86172000 hold"), False),
        ("substituted identity: settled object", mutate(base, r"SETTLE object=1", "SETTLE object=3"), False),
        ("substituted identity: observer task", mutate(base, r"tid=9401 step=O_NEW", "tid=9402 step=O_NEW"), False),
        ("substituted identity: observer CPU", mutate(base, r"cpu=1 tid=9401 step=O_NEW", "cpu=0 tid=9401 step=O_NEW"), False),
        ("substituted identity: window", mutate(base, r"asid=4 va=0x20080000", "asid=4 va=0x20081000"), False),
        ("kernel verdict alone is not proof", [l for l in base if "SRR_USER" not in l], False),
    ]
    bad = 0
    for name, lines, want in cases:
        fails = grade(lines)
        ok = (not fails) == want
        bad += 0 if ok else 1
        print(f"SRR_GRADER_FIXTURE {'ok' if ok else 'WRONG'} expect={'pass' if want else 'fail'} case={name!r}"
              + ("" if want else f" first_reason={fails[0] if fails else None!r}"))
    print(f"SRR_GRADER_SELF_TEST cases={len(cases)} wrong={bad} result={'ok' if bad == 0 else 'fail'}")
    return bad == 0


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "--self-test":
        sys.exit(0 if self_test() else 1)
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    lines = open(sys.argv[1], errors="replace").read().split("\n")
    fails = grade(lines)
    for f in fails:
        print(f"[srr-witness][fail] {f}")
    if fails:
        print("SRR_WITNESS_SEAL arch=riscv64 result=fail")
        sys.exit(1)
    print("SRR_WITNESS_SEAL arch=riscv64 requester_cpu=0 remote_target=cpu1 fence=ok install=complete "
          "settle=reclaimed observer=credited bytes=granted order=causal result=ok")


if __name__ == "__main__":
    main()
