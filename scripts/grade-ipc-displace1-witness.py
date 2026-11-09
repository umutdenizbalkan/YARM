#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""IPC-DISPLACE1-ACCEPTANCE / -MARKER1 — grade one live competing-receiver witness boot.

Input: the normalized serial log of a boot with `yarm.ipc_displace1_witness=1`.

Every claim is checked against the KERNEL's own markers, correlated with the cell's userspace
lines by task identity, endpoint identity and wait generation, never by marker presence alone:

  per round k (in log order)
    R1 blocks on the production split route   IPC_RECV_BLOCK_SPLIT_DONE tid=R1 ... wait_gen=W result=blocked
    R2 is refused by the publication owner    WA3C2_WAITER_PUBLISH_REFUSED ... new_tid=R2 ... live_tid=R1 live_wait_gen=W
                                              D2_RECV_WAITER_REFUSED ... tid=R2 ... live_tid=R1 ... reason=receiver_already_waiting
    R2 is unwound exactly and answered        D2_RECV_SPLIT_UNWIND_OK cpu=C tid=R2 ... via=exact ... status=running placement=current
                                              IPC_RECV_BLOCK_SPLIT_SETTLED cpu=C tid=R2 ... reason=receiver_already_waiting answer=6 outcome=restored resumable=1
                                              (MARKER1: a settled refusal is NOT fail-closed and is NOT a publish race, so
                                               IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED and D2_PUBLISH_RACE_UNWIND must not appear)
    R2 runs again and reports QueueFull       IPCD1_WITNESS_R2 round=k answer=6 canary_ok=1
    S's message reaches R1 exactly once       IPC_RECV_DELIVER_TO_WAITER tid=R1 endpoint=E ...
                                              IPCD1_WITNESS_R1 round=k answer=256 ... ok=1

  across the boot
    one endpoint incarnation for both receivers (IPC_RECV_AFTER_CAP_OK index + generation)
    no displacement of one identity by another, no broad-route refusal, no waiter-ownership
    or acknowledgement-store failure marker, no fatal condition
    R2 receives normally once R1 has left (the final phase), so the endpoint is clean

Placement is read from the kernel's `cpu=` fields only. A round is cross-CPU when R1's block
and R2's refusal were observed on different CPUs; on a multi-CPU boot that count is reported,
and a cross-CPU claim is made only when it is non-zero.

Exit status 0 = sealed, 1 = not sealed.
"""
import argparse
import re
import sys

ANSWER_QUEUE_FULL = 6
ANSWER_MESSAGE = 256


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s]+)", line))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log")
    ap.add_argument("--arch", required=True)
    ap.add_argument("--smp", type=int, default=1)
    ap.add_argument("--rounds", type=int, default=16)
    ap.add_argument("--core-rc", type=int, default=None)
    ap.add_argument("--core-log", action="append", default=[])
    a = ap.parse_args()

    with open(a.log, "rb") as fh:
        lines = [l.decode("utf-8", "replace").rstrip("\n") for l in fh]

    fails = []

    def fail(msg):
        fails.append(msg)

    # ── the cell's own lines ──
    begin = [(i, l) for i, l in enumerate(lines) if "IPCD1_WITNESS_BEGIN " in l]
    if len(begin) != 1:
        fail(f"witness BEGIN count={len(begin)} (want 1)")
        print_seal(a, fails, {})
        return 1
    b = kv(begin[0][1].split("msg=", 1)[-1])
    r1, r2, s = b.get("r1_tid"), b.get("r2_tid"), b.get("s_tid")
    rounds = int(b.get("rounds", "0"))
    if rounds != a.rounds:
        fail(f"cell rounds={rounds}, schedule rounds={a.rounds}")
    if len({r1, r2, s}) != 3:
        fail(f"R1/R2/S are not three distinct tasks: r1={r1} r2={r2} s={s}")

    slots = [kv(l) for l in lines if "SHARED_REGION_ORACLE_SLOTS " in l]
    if not slots or slots[0].get("slot5") != "17":
        fail("init slot 5 was not armed with selector 17")
    eidx = slots[0].get("eidx") if slots else None

    def user(tag, tid):
        out = []
        for i, l in enumerate(lines):
            if f"USER_LOG tid={tid} msg={tag} " in l:
                out.append((i, kv(l.split("msg=", 1)[1])))
        return out

    u_rounds = user("IPCD1_WITNESS_ROUND", s)
    u_r1 = user("IPCD1_WITNESS_R1", r1)
    u_r2 = user("IPCD1_WITNESS_R2", r2)
    summary = [kv(l.split("msg=", 1)[1]) for l in lines if "msg=IPCD1_WITNESS rounds_ok=" in l]
    done = [l for l in lines if "msg=IPCD1_WITNESS_DONE " in l]
    wfail = [l for l in lines if "msg=IPCD1_WITNESS_FAIL " in l]
    for l in wfail:
        fail("cell reported " + l.split("msg=", 1)[1])
    if [int(d["round"]) for _, d in u_rounds] != list(range(1, rounds + 1)):
        fail(f"ROUND lines {[d['round'] for _, d in u_rounds]} are not exactly 1..{rounds}")
    r1_by_round = {}
    for _, d in u_r1:
        r1_by_round.setdefault(int(d["round"]), []).append(d)
    for k in range(1, rounds + 1):
        got = r1_by_round.get(k, [])
        if len(got) != 1:
            fail(f"round {k}: R1 resumed {len(got)} times (want exactly 1)")
        elif got[0].get("answer") != str(ANSWER_MESSAGE) or got[0].get("ok") != "1":
            fail(f"round {k}: R1 did not receive its own message from S: {got[0]}")
    extra = sorted(set(r1_by_round) - set(range(1, rounds + 1)))
    if extra:
        fail(f"R1 resumed in unexpected rounds {extra}")
    r2_by_round = {}
    for _, d in u_r2:
        r2_by_round.setdefault(int(d["round"]), []).append(d)
    for k in range(1, rounds + 1):
        got = r2_by_round.get(k, [])
        if len(got) != 1:
            fail(f"round {k}: R2 returned {len(got)} times (want exactly 1)")
        elif got[0].get("answer") != str(ANSWER_QUEUE_FULL) or got[0].get("canary_ok") != "1":
            fail(f"round {k}: R2 was not answered QueueFull on its own stack: {got[0]}")
    fin = r2_by_round.get(rounds + 1, [])
    if len(fin) != 1 or fin[0].get("answer") != str(ANSWER_MESSAGE):
        fail(f"final phase: R2 did not receive normally once R1 left: {fin}")
    if len(summary) != 1 or summary[0].get("result") != "1":
        fail(f"cell summary is not result=1: {summary}")
    if len(done) != 1 or "result=ok" not in done[0]:
        fail(f"cell completion is not result=ok: {done}")

    # ── the kernel's markers ──
    def k_lines(tag):
        return [(i, kv(l)) for i, l in enumerate(lines) if l.startswith(tag + " ") or (" " + tag + " ") in l]

    caps = {}
    for i, l in enumerate(lines):
        m = re.search(r"IPC_RECV_AFTER_CAP_OK tid=(\d+) cap=\d+ endpoint=Endpoint \{ index: (\d+), generation: (\d+) \}", l)
        if m and m.group(1) in (r1, r2):
            caps.setdefault(m.group(1), set()).add((m.group(2), m.group(3)))
    if set(caps) != {r1, r2}:
        fail(f"no endpoint identity observed for both receivers: {caps}")
    else:
        ids = caps[r1] | caps[r2]
        if len(ids) != 1:
            fail(f"R1 and R2 did not reference one endpoint incarnation: {ids}")
        elif eidx is not None and next(iter(ids))[0] != eidx:
            fail(f"receivers used endpoint {ids}, the cell's endpoint is index {eidx}")
    ep_index = next(iter(caps.get(r1, {("?", "?")})))[0]
    ep_gen = next(iter(caps.get(r1, {("?", "?")})))[1]

    blocks = [(i, d) for i, d in k_lines("IPC_RECV_BLOCK_SPLIT_DONE") if d.get("tid") == r1 and d.get("result") == "blocked"]
    r2_blocks = [(i, d) for i, d in k_lines("IPC_RECV_BLOCK_SPLIT_DONE") if d.get("tid") == r2]
    wa3 = [(i, d) for i, d in k_lines("WA3C2_WAITER_PUBLISH_REFUSED") if d.get("new_tid") == r2 and d.get("reason") == "live_receiver_armed"]
    refused = [(i, d) for i, d in k_lines("D2_RECV_WAITER_REFUSED") if d.get("tid") == r2]
    unwound = [(i, d) for i, d in k_lines("D2_RECV_SPLIT_UNWIND_OK") if d.get("tid") == r2]
    closed = [(i, d) for i, d in k_lines("IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED")]
    settled = [(i, d) for i, d in k_lines("IPC_RECV_BLOCK_SPLIT_SETTLED") if d.get("tid") == r2 and d.get("reason") == "receiver_already_waiting"]
    deliver_r1 = [(i, d) for i, d in k_lines("IPC_RECV_DELIVER_TO_WAITER") if d.get("tid") == r1]
    deliver_r2 = [(i, d) for i, d in k_lines("IPC_RECV_DELIVER_TO_WAITER") if d.get("tid") == r2]
    wake_r1 = [(i, d) for i, d in k_lines("SCHED_WAKE_ENQUEUE") if d.get("tid") == r1]
    wake_r2 = [(i, d) for i, d in k_lines("SCHED_WAKE_ENQUEUE") if d.get("tid") == r2]
    race_unwinds = [(i, d) for i, d in k_lines("D2_PUBLISH_RACE_UNWIND")]

    def want(name, got, n):
        if len(got) != n:
            fail(f"kernel {name}: {len(got)} (want {n})")

    want("R1 split blocks", blocks, rounds)
    want("publication-owner refusals of R2 (WA3C2)", wa3, rounds)
    want("refusals of R2 (D2_RECV_WAITER_REFUSED)", refused, rounds)
    want("receiver_already_waiting settles of R2", settled, rounds)
    want("exact unwinds of R2", unwound, rounds)
    # MARKER1: a settled refusal is not a fail-closed settlement, and this boot has no queue race.
    # Either marker here is a real failure (an unwind that did not restore, an InvalidEndpoint, a
    # race the witness never creates), never something to explain away.
    if closed:
        fail(f"{len(closed)} fail-closed receive settlement(s): {[d for _, d in closed][:3]}")
    want("deliveries to R1", deliver_r1, rounds)
    if len(r2_blocks) != 1:
        fail(f"kernel R2 split blocks: {len(r2_blocks)} (want 1: the final phase only)")
    for _, d in unwound:
        if d.get("via") != "exact" or d.get("status") != "running" or d.get("placement") != "current":
            fail(f"R2's unwind did not restore it running and current: {d}")
    for _, d in refused:
        if d.get("live_tid") != r1 or d.get("reason") != "receiver_already_waiting" or d.get("endpoint") != ep_index:
            fail(f"a refusal of R2 did not name R1 on endpoint {ep_index}: {d}")
    for _, d in settled:
        if d.get("answer") != str(ANSWER_QUEUE_FULL) or d.get("outcome") != "restored" or d.get("resumable") != "1":
            fail(f"R2's settlement was not QueueFull through its own frame: {d}")
    for _, d in deliver_r1 + deliver_r2:
        if d.get("endpoint") != ep_index:
            fail(f"a delivery used another endpoint: {d}")
    if len(deliver_r2) != 1:
        fail(f"deliveries to R2: {len(deliver_r2)} (want 1: the final phase only)")
    # R1 is woken once per round, on its HOME CPU (where it blocked); R2 is never woken by IPC
    # during the refusal rounds (no spurious dispatch), only by the final phase's delivery.
    want("IPC wakes of R1", wake_r1, rounds)
    r1_home = {d.get("cpu") for _, d in blocks}
    for _, d in wake_r1:
        if d.get("reason") != "pinned" or d.get("cpu") not in r1_home or len(r1_home) != 1:
            fail(f"R1 was not woken on its home CPU {sorted(r1_home)}: {d}")
    if len(wake_r2) != 1 or (deliver_r2 and wake_r2 and wake_r2[0][0] < deliver_r2[0][0]):
        fail(f"IPC wakes of R2: {len(wake_r2)} (want 1, after the final delivery)")
    # MARKER1: the publish-race pair now names the queue-publication race only. S sends only after
    # R2 has returned and R1 is blocked, so this boot has none: any occurrence is unexplained.
    if race_unwinds:
        fail(f"{len(race_unwinds)} publish-race unwind(s) on a boot with no queue race: {[d for _, d in race_unwinds][:3]}")

    # Per-round causal chain, in log order: R1 blocks, then R2 is refused naming THAT block,
    # then the round's message is delivered to R1.
    cross = 0
    per_round = []
    n = min(len(blocks), len(wa3), len(refused), len(unwound), len(settled), len(deliver_r1))
    for k in range(n):
        bi, bd = blocks[k]
        wi, wd = wa3[k]
        ri, rd = refused[k]
        ui, ud = unwound[k]
        ci, cd = settled[k]
        di, dd = deliver_r1[k]
        # The settlement is only authorized by the exact unwind that precedes it, on the same CPU.
        if not (bi < wi <= ri < ui < ci < di):
            fail(f"round {k + 1}: order block={bi} owner_refusal={wi} refusal={ri} unwind={ui} settle={ci} delivery={di}")
        if ud.get("cpu") != cd.get("cpu"):
            fail(f"round {k + 1}: R2 unwound on cpu {ud.get('cpu')} but settled on cpu {cd.get('cpu')}")
        if k < len(wake_r1) and not (di < wake_r1[k][0]):
            fail(f"round {k + 1}: R1's wake precedes its delivery")
        if wd.get("live_wait_gen") != bd.get("wait_gen"):
            fail(f"round {k + 1}: refusal names live_wait_gen={wd.get('live_wait_gen')}, R1 blocked with wait_gen={bd.get('wait_gen')}")
        nxt = blocks[k + 1][0] if k + 1 < len(blocks) else len(lines)
        if not di < nxt:
            fail(f"round {k + 1}: delivery is not before R1's next block")
        r1cpu, r2cpu = bd.get("cpu"), cd.get("cpu")
        per_round.append((r1cpu, r2cpu))
        if r1cpu != r2cpu:
            cross += 1

    # ── what must never appear ──
    for i, l in enumerate(lines):
        m = re.search(r"D2_RECV_WAITER_DISPLACED endpoint=(\d+) old_tid=(\d+) old_asid=(\d+) old_wait_gen=\d+ new_tid=(\d+) new_asid=(\d+)", l)
        if m and (m.group(2), m.group(3)) != (m.group(4), m.group(5)):
            fail(f"a receiver was DISPLACED by another identity: {l.strip()}")
    for bad in ("D2_RECV_WAITER_REFUSED_UNWIND", "WA3C2_WAITER_ARM_REFUSED", "WA3C2_WAITER_CLAIM_REFUSED",
                "WA3C2_WAITER_CANCEL_FAILED", "WA3C2_WAITER_CONSUME_FAILED", "WA3C2_WAITER_RESTORE_FAILED",
                "WA3C2_WAITER_RESTORE_UNOWNED", "DIRECT_ACK_STORE_CAPACITY", "YARM_SPLIT_DISPATCH_FALLBACK",
                "KERNEL PANIC", "panicked at", "UNHANDLED"):
        hits = sum(1 for l in lines if bad in l)
        if hits:
            fail(f"{hits} line(s) with {bad}")

    facts = {
        "r1": r1, "r2": r2, "s": s, "endpoint": f"{ep_index}/{ep_gen}",
        "blocks": len(blocks), "refusals": len(refused), "deliveries_r1": len(deliver_r1),
        "wakes_r1": len(wake_r1), "r1_wake_cpus": ",".join(sorted({d.get("cpu") for _, d in wake_r1})) or "-",
        "r1_cpus": ",".join(sorted({c for c, _ in per_round})) or "-",
        "r2_cpus": ",".join(sorted({c for _, c in per_round})) or "-",
        "cross_cpu_rounds": cross,
    }
    # The standard core smoke ran this boot. Its verdict is reported SEPARATELY from the witness
    # and is never inferred from its exit status alone: the RISC-V smoke exits 0 in non-strict
    # mode even when a check failed. Every `[fail]`/`[error]` line it printed is a rejection.
    witness_ok = not fails
    core_reasons = []
    for path in a.core_log:
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                for l in fh:
                    t = l.strip()
                    if (t.startswith("[fail]") or t.startswith("[error]")) and "check(s) failed" not in t \
                            and "smoke FAILED" not in t and "witness FAILED" not in t:
                        core_reasons.append(t)
        except OSError:
            pass
    core_reasons = sorted(set(core_reasons))
    core_ok = (a.core_rc in (None, 0)) and not core_reasons
    facts["witness"] = "ok" if witness_ok else "fail"
    facts["core_smoke"] = "ok" if core_ok else f"rejected(rc={a.core_rc})"
    for r in core_reasons:
        print(f"[ipc-displace1-grade][core] {r}")
    if not core_ok:
        fail("the standard core smoke rejected this boot (see [core] lines)")
    print_seal(a, fails, facts)
    return 1 if fails else 0


def print_seal(a, fails, facts):
    for f in fails:
        print(f"[ipc-displace1-grade][fail] {f}")
    extra = " ".join(f"{k}={v}" for k, v in facts.items())
    if a.smp > 1 and facts.get("cross_cpu_rounds", 0) == 0 and not fails:
        print("[ipc-displace1-grade][note] multi-CPU boot, but no round placed R1 and R2 on different CPUs: cross-CPU UNCOVERED")
    print(f"IPC_DISPLACE1_WITNESS_SEAL arch={a.arch} smp={a.smp} rounds={a.rounds} {extra} "
          f"result={'fail' if fails else 'ok'}")


if __name__ == "__main__":
    sys.exit(main())
