#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""BUILD-GATES1 §4 — boot-log fixtures for the AArch64 U9-RX4 reply-terminal grader.

Each case is a log built from the production records an AArch64 core boot emits for tid 2's PM
lifecycle query (the line formats are the kernel's own; the fresh cases reproduce real boots).
The witnessed transaction is identified by the production records themselves: the reply record
(`record_index`, `record_generation`) the request route committed and the terminal armed, and the
one-shot cap the kernel bound to that record. A record slot is shared with other callers, so its
generation is whatever the allocator reached: 1 when tid 2 calls first, later once init's calls
have reused the slot. `cases.tsv` lists `name<TAB>accept|reject<TAB>reason-substring`.

    mkcases.py --out DIR
"""
import argparse
import os
import sys

CASES = {}


def case(name, expect, reason=""):
    def deco(fn):
        CASES[name] = (expect, reason, fn)
        return fn
    return deco


def rec(r, g):
    return f"record_index={r} record_generation={g}"


def expected(r, g):
    return f"expected=Reply {{ index: {r}, generation: {g} }}"


def init_call(r, g, cap):
    """An unrelated transaction on the same slot: init (tid 1) calls PM directly and PM replies."""
    return [
        f"IPCCALL_DIRECT_SPLIT_OK caller_tid=1 server_tid=3 endpoint=0 endpoint_generation=1 server_reply_cap={cap} {rec(r, g)} len=50 result=ok",
        f"IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=1 caller_asid=1 {rec(r, g)} replier_tid=3 blocked_recv_generation={g} finite_deadline=0 deadline_reserved=0 result=ok",
        f"IPC_REPLY_OBJECT_OK tid=3 cap={cap} reply_index={r} generation={g} target_endpoint=1",
        f"IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=1 replier_tid=3 cap={cap} waiter_cap={cap} {expected(r, g)} ok=true",
        f"IPC_REPLY_DIRECT_AUTHORITY_RECLAIMED {rec(r, g)} replier_tid=3 caller_tid=1 replier_ok=1 caller_ok=0 result=ok",
        f"IPCREPLY_DIRECT_TERMINAL_CLAIM {rec(r, g)} replier_tid=3 terminal=Reply resolution=commit settled=1 result=ok",
    ]


def queued_request(r, g, cap, alias=65541):
    """tid 2's query queued for a busy PM; PM's one-shot is bound to the record on its receive."""
    return [
        f"IPCCALL_QUEUED_SPLIT_OK tid=2 endpoint=0 endpoint_generation=1 reply_cap={alias} {rec(r, g)} len=10 result=ok",
        f"IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 {rec(r, g)} replier_tid=0 blocked_recv_generation=1 finite_deadline=1 deadline_reserved=1 result=ok",
        f"IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 {rec(r, g)} terminal_epoch=1 token_slot=0 token_generation=1 deadline=4 result=ok",
    ]


def queued_receive(r, g, cap):
    return [
        f"IPC_RECV_REPLY_CAP_RESPONDER_BOUND reply_index={r} reply_gen={g} responder_tid=3 responder_asid=3 result=ok",
        f"IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index={r} reply_gen={g} cap={cap}",
        f"YARM_D5_SPLIT_RECORD reply_index={r} reply_gen={g} cap={cap}",
        f"IPC_REPLY_CAP_ONESHOT_OK receiver_tid=3 local_reply_cap={cap}",
        f"USER_LOG tid=3 msg=PM_RECV_GOT_MSG opcode=12 len=8 reply_cap=Some({cap}) transferred_cap=None sender_tid=2",
    ]


def direct_request(r, g, cap):
    """tid 2's query copied straight into a PM already blocked in its receive (NR 6)."""
    return [
        f"IPCCALL_DIRECT_SPLIT_OK caller_tid=2 server_tid=3 endpoint=0 endpoint_generation=1 server_reply_cap={cap} {rec(r, g)} len=10 result=ok",
        f"IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 {rec(r, g)} replier_tid=3 blocked_recv_generation={g} finite_deadline=1 deadline_reserved=1 result=ok",
        f"IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 {rec(r, g)} terminal_epoch=1 token_slot=0 token_generation=1 deadline=9 result=ok",
        f"USER_LOG tid=3 msg=PM_RECV_GOT_MSG opcode=12 len=8 reply_cap=Some({cap}) transferred_cap=None sender_tid=2",
    ]


PM = [
    "USER_LOG tid=3 msg=PM_LIFECYCLE_QUERY_RECV tid=2",
    "USER_LOG tid=3 msg=PM_LIFECYCLE_QUERY_REPLY tid=2 found=1 image_id=1",
]


def reply_win(r, g, cap, alias=65541, route="queued"):
    out = [
        "IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 len=40",
        f"IPC_REPLY_OBJECT_OK tid=3 cap={cap} reply_index={r} generation={g} target_endpoint=5",
        f"IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap={cap} waiter_cap={cap} {expected(r, g)} ok=true",
    ]
    if route == "queued":
        out.append(f"IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2 cap={alias} {expected(r, g)} ok=true")
    out += [
        f"IPC_REPLY_DIRECT_AUTHORITY_RECLAIMED {rec(r, g)} replier_tid=3 caller_tid=2 replier_ok=1 caller_ok={1 if route == 'queued' else 0} result=ok",
        f"IPCREPLY_DIRECT_TERMINAL_CLAIM {rec(r, g)} replier_tid=3 terminal=Reply resolution=commit settled=1 result=ok",
    ]
    return out


TIMEOUT = [
    "IPC_REPLY_TIMEOUT_DEFERRED arch=aarch64 published=1 drained=1 result=ok",
    "IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok",
    "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok",
]


def prelock_refusal(r, g):
    return [f"IPCREPLY_DIRECT_REFUSED_PRE_LOCK {rec(r, g)} replier_tid=3 terminal=Settled reply_copies=0 caller_wakes=0 mutations=0 err=WrongObject result=ok"]


def fresh_queued_reply(r=0, g=1, cap=65538):
    return queued_request(r, g, cap) + queued_receive(r, g, cap) + reply_win(r, g, cap) + PM


# A slot reused by init twice before tid 2 calls: tid 2's record is (0, 3).
PRIOR = init_call(0, 1, 65538) + init_call(0, 2, 131074)


@case("fresh_reply_win", "accept")
def _():
    # A real AArch64 boot (blr1 xfer2 ord1 off1): tid 2 calls first, record (0, 1), reply wins.
    return fresh_queued_reply() + init_call(0, 2, 196610)


@case("fresh_timeout_win_prelock", "accept")
def _():
    # A real AArch64 boot (blr1 lock2): the deadline wins; the late reply is refused pre-lock.
    return (queued_request(0, 1, 65538) + TIMEOUT + queued_receive(0, 1, 65538) + PM
            + prelock_refusal(0, 1) + init_call(1, 1, 65539))


@case("fresh_timeout_win_legacy", "accept")
def _():
    return (queued_request(0, 1, 65538) + TIMEOUT + queued_receive(0, 1, 65538) + PM
            + ["IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject"])


@case("reuse_reply_win_queued", "accept")
def _():
    return PRIOR + fresh_queued_reply(0, 3, 196610) + init_call(0, 4, 262146)


@case("reuse_reply_win_direct", "accept")
def _():
    return (PRIOR + direct_request(0, 3, 196610) + PM
            + reply_win(0, 3, 196610, route="direct") + init_call(0, 4, 262146))


@case("reuse_timeout_win", "accept")
def _():
    return (PRIOR + queued_request(0, 3, 196610) + TIMEOUT + queued_receive(0, 3, 196610) + PM
            + prelock_refusal(0, 3))


@case("reuse_reply_win_alongside_later_transaction_on_slot", "accept")
def _():
    # Another transaction's reply on the same slot, after tid 2's settled, is not tid 2's.
    return PRIOR + fresh_queued_reply(0, 3, 196610) + init_call(0, 4, 262146) + init_call(0, 5, 327682)


@case("stale_generation_reply", "reject", "resolved against another record")
def _():
    # tid 2's record is (0, 3); the reply resolves and commits against the stale generation 1.
    lines = PRIOR + queued_request(0, 3, 196610) + queued_receive(0, 3, 196610) + PM
    lines += [l.replace("generation=3", "generation=1").replace("generation: 3", "generation: 1")
              for l in reply_win(0, 3, 196610)]
    return lines


@case("stale_generation_route", "reject", "exactly one route")
def _():
    # The arming names (0, 3) but the request route that carries tid 2's query committed (0, 1).
    lines = PRIOR + queued_request(0, 3, 196610) + queued_receive(0, 3, 196610) + PM + reply_win(0, 3, 196610)
    lines[len(PRIOR)] = lines[len(PRIOR)].replace("record_generation=3", "record_generation=1")
    return lines


@case("another_transaction_on_the_same_slot", "reject", "exactly one claimant must win")
def _():
    # tid 2's (0, 3) never completes; init's later (0, 4) reply on the same slot does.
    return (PRIOR + queued_request(0, 3, 196610) + queued_receive(0, 3, 196610) + PM
            + ["IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 len=40"] + init_call(0, 4, 262146))


@case("substituted_reply_identity", "reject", "resolved against another record")
def _():
    # PM's cap resolves against another slot's record (1, 3) rather than the request's own.
    lines = PRIOR + fresh_queued_reply(0, 3, 196610)
    return [l.replace("cap=196610 reply_index=0 generation=3", "cap=196610 reply_index=1 generation=3")
            for l in lines]


@case("substituted_request_identity", "reject", "is bound to record")
def _():
    # PM receives a cap the kernel bound to another record than tid 2's request committed.
    lines = PRIOR + fresh_queued_reply(0, 3, 196610)
    return [l.replace("WAITER_CAP_SET reply_index=0 reply_gen=3", "WAITER_CAP_SET reply_index=0 reply_gen=2")
            for l in lines]


@case("substituted_direct_cap", "reject", "exactly one route")
def _():
    # The direct route committed server cap 196610, but PM's receive delivered another cap.
    lines = PRIOR + direct_request(0, 3, 196610) + PM + reply_win(0, 3, 262146, route="direct")
    return [l.replace("reply_cap=Some(196610)", "reply_cap=Some(262146)") for l in lines]


@case("duplicate_completion_reply_commit", "reject", "the terminal must settle exactly once")
def _():
    lines = PRIOR + fresh_queued_reply(0, 3, 196610)
    return lines + [f"IPCREPLY_DIRECT_TERMINAL_CLAIM {rec(0, 3)} replier_tid=3 terminal=Reply resolution=commit settled=1 result=ok"]


@case("duplicate_completion_both_winners", "reject", "exactly one claimant must win")
def _():
    return PRIOR + fresh_queued_reply(0, 3, 196610) + TIMEOUT


@case("duplicate_resolution", "reject", "resolved more than once")
def _():
    lines = PRIOR + fresh_queued_reply(0, 3, 196610)
    return lines + ["IPC_REPLY_OBJECT_OK tid=3 cap=196610 reply_index=0 generation=3 target_endpoint=5"]


@case("missing_completion", "reject", "exactly one claimant must win")
def _():
    return PRIOR + queued_request(0, 3, 196610) + queued_receive(0, 3, 196610) + PM


@case("missing_completion_fresh", "reject", "exactly one claimant must win")
def _():
    return queued_request(0, 1, 65538) + queued_receive(0, 1, 65538) + PM


@case("two_armings_for_the_caller", "reject", "armed exactly once")
def _():
    return PRIOR + queued_request(0, 3, 196610) + fresh_queued_reply(0, 4, 262146)


@case("reuse_reply_win_wrong_revoke_identity", "reject", "revoked once")
def _():
    lines = PRIOR + fresh_queued_reply(0, 3, 196610)
    return [l.replace("cap=196610 waiter_cap=196610 expected=Reply { index: 0, generation: 3 }",
                      "cap=196610 waiter_cap=196610 expected=Reply { index: 0, generation: 1 }")
            for l in lines]


@case("reuse_timeout_win_prelock_refusal_on_stale_generation", "reject", "must be refused")
def _():
    return (PRIOR + queued_request(0, 3, 196610) + TIMEOUT + queued_receive(0, 3, 196610) + PM
            + prelock_refusal(0, 1))


@case("fresh_substituted_reply_index", "reject", "resolved against another record")
def _():
    # Generation 1, but PM's cap resolves against slot 1 rather than the request's slot 0.
    return [l.replace("cap=65538 reply_index=0 generation=1", "cap=65538 reply_index=1 generation=1")
            for l in fresh_queued_reply()]


@case("fresh_wrong_revoke_identity", "reject", "revoked once")
def _():
    # The replier-side revoke names another record than the one the request committed.
    return [l.replace("waiter_cap=65538 expected=Reply { index: 0, generation: 1 }",
                      "waiter_cap=65538 expected=Reply { index: 1, generation: 1 }")
            for l in fresh_queued_reply()]


@case("extra_revoke_of_pm_cap_against_another_record", "reject", "revoked once, against")
def _():
    # The witnessed revoke is present, and PM's cap is ALSO revoked as another record's one-shot.
    return fresh_queued_reply() + [
        f"IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=65538 waiter_cap=65538 {expected(1, 1)} ok=true"]


# ── the evaluator's original self-test cases (U9-RX4 / BL4b / DIRECT3-CAP-FINAL), kept ──────

def fresh_timeout(*late):
    return queued_request(0, 1, 65538) + TIMEOUT + queued_receive(0, 1, 65538) + PM + list(late)


@case("selftest_winner_without_its_attributed_wake", "reject", "the caller must wake exactly once")
def _():
    return [l for l in fresh_queued_reply() if not l.startswith("IPC_RECV_V2_META_BLOCKED_WAITER_OK")]


@case("selftest_reply_success_after_timeout", "reject", "exactly one claimant must win")
def _():
    return fresh_timeout("IPC_REPLY_OBJECT_OK tid=3 cap=65538 reply_index=0 generation=1 target_endpoint=5")


@case("selftest_reply_completion_with_timeout_ownership", "reject", "the reply must NOT complete the caller")
def _():
    return fresh_timeout("IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject",
                         "IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 len=40")


@case("selftest_late_reply_successes_nonzero", "reject", "late_reply_successes must be 0")
def _():
    return [l.replace("late_reply_successes=0", "late_reply_successes=1")
            for l in fresh_timeout("IPC_REPLY_FAIL tid=3 reply_cap=65538 err=WrongObject")]


@case("selftest_timeout_win_without_refusal_of_the_late_reply", "reject", "must be refused")
def _():
    return fresh_timeout()


@case("selftest_leaked_alias_record_link", "reject", "leak on the witnessed path")
def _():
    return fresh_queued_reply() + ["IPC_REPLY_FAST_REVOKE_FAIL tid=3"]


@case("selftest_prelock_refusal_that_mutated_state", "reject", "was not inert")
def _():
    return fresh_timeout(prelock_refusal(0, 1)[0].replace("reply_copies=0", "reply_copies=1")
                         .replace("mutations=0", "mutations=1"))


@case("selftest_prelock_refusal_on_another_record", "reject", "must be refused")
def _():
    return fresh_timeout(prelock_refusal(7, 1)[0])


@case("selftest_reply_win_with_a_renumbered_cap", "accept")
def _():
    return [l.replace("65538", "262146") for l in fresh_queued_reply()]


@case("selftest_resolve_and_revoke_name_another_cap", "reject", "exactly one claimant must win")
def _():
    lines = fresh_queued_reply()
    return [l.replace("cap=65538 reply_index", "cap=131074 reply_index")
            .replace("replier_tid=3 cap=65538 waiter_cap=65538", "replier_tid=3 cap=131074 waiter_cap=131074")
            for l in lines]


@case("selftest_reply_cap_minted_after_the_receive", "reject", "before the receive")
def _():
    lines = fresh_queued_reply()
    mint = next(l for l in lines if l.startswith("IPC_REPLY_CAP_ONESHOT_OK"))
    lines.remove(mint)
    at = next(i for i, l in enumerate(lines) if "PM_RECV_GOT_MSG" in l)
    return lines[:at + 1] + [mint] + lines[at + 1:]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.out, "cases.tsv"), "w") as m:
        for name, (expect, reason, fn) in CASES.items():
            with open(os.path.join(a.out, f"{name}.log"), "w") as f:
                f.write("\n".join(fn()) + "\n")
            m.write(f"{name}\t{expect}\t{reason}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
