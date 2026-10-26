#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# U9-RX4 TERMINAL-OUTCOME EVALUATOR (AArch64; claimant-aware, identity-scoped).
#
# Sourced by `scripts/qemu-aarch64-core-smoke.sh`, which grades every boot with it, and by
# `tests/aarch64_reply_grader.rs`, which runs it over the fixture corpus
# (`tests/fixtures/u9rx4/mkcases.py`). One implementation: the smoke's self-test runs the same
# corpus before the real log is judged.
#
# The witnessed exchange arms a FOUR-TICK reply deadline on tid 2's PM lifecycle query, in
# every boot. That makes TWO outcomes legitimate, and which one occurs is a genuine race:
#
#   REPLY WINS    PM answers inside four ticks. The reply claims and commits the terminal,
#                 both sides of the one-shot are revoked, and the caller resumes via the reply.
#   TIMEOUT WINS  PM does not. The deadline claims and commits the SAME terminal, wakes the
#                 caller exactly once, invalidates the reply aliases, and every later reply is
#                 refused.
#
# It requires exactly one winner, exactly one caller wake attributed to THAT winner, and exactly
# one terminal settlement, and it holds the losing claimant to having changed nothing. It fails a
# genuinely lost reply (no winner, no wake), two winners, a winner without its wake, a reply that
# succeeds after a timeout, and a leaked alias/record/link.
#
# WHICH transaction it judges (BUILD-GATES1 §4). Every anchor is scoped to the witnessed
# transaction, never to boot-global counts that other replies also satisfy, and the
# transaction's identity is READ from the production records rather than assumed:
#
#   * the caller is tid 2 and the replier PM (tid 3);
#   * the reply RECORD is `(record_index, record_generation)` from the one arming of tid 2's
#     terminal with a finite deadline. A record slot is shared with every other caller of PM, so
#     its generation is whatever the allocator reached: 1 when tid 2 calls first, a later value
#     once init's calls have reused the slot (seen on x86 at -smp 4/8 in KERNEL-NOFP1). This
#     evaluator used to pin generation 1 on the arming, the resolve, the commit and the refusals,
#     so a valid reuse was graded as "never armed", and its resolve matched ANY record index;
#   * exactly ONE request route must have committed that exact record for tid 2: queued
#     (`IPCCALL_QUEUED_SPLIT_OK`, PM busy; PM's one-shot is bound to the record when its receive
#     takes the message, `IPC_RECV_REPLY_CAP_WAITER_CAP_SET`) or direct NR 6
#     (`IPCCALL_DIRECT_SPLIT_OK`, PM already blocked; the route names PM's cap itself) — the same
#     derivation as the repaired x86 check;
#   * PM's one-shot CAP is read from the one receive of tid 2's query and must be the cap the
#     kernel bound to that record;
#   * the reply resolution, the terminal commit, both revokes and any refusal must name that exact
#     record and generation, and PM's cap may resolve against no other record.
#
# An unrelated transaction on the same slot (another generation) therefore neither satisfies nor
# disturbs the evaluation, and a stale generation or a substituted identity is refused by name.

_u9_c()   { printf '%s\n' "$_U9_LOG" | rg -a -F -c -- "$1" 2>/dev/null || printf '0'; }
_u9_cre() { printf '%s\n' "$_U9_LOG" | rg -a -c -e "$1" 2>/dev/null || printf '0'; }
# First line matching <regex>, then the numeric value of <key>= on it ('' when absent).
_u9_field() {
  printf '%s\n' "$_U9_LOG" | rg -a -m1 -e "$1" 2>/dev/null | rg -o -m1 -e "(^| )$2=[0-9]+" 2>/dev/null | cut -d= -f2
}
# Line number of the first line matching <regex> ('' when absent).
_u9_line() { printf '%s\n' "$_U9_LOG" | rg -a -n -m1 -e "$1" 2>/dev/null | cut -d: -f1; }

# Evaluate one boot log's terminal outcome. Echoes [ok]/[error] lines; returns 0 on accept.
u9rx4_eval_terminal() {
  _U9_LOG="$1"
  local tag="${2:-U9-RX4}" bad=0

  # The witnessed receive and its cap: exactly one receive of tid 2's query, carrying a reply cap.
  local got_re='PM_RECV_GOT_MSG opcode=12 len=8 reply_cap=Some\([0-9]+\) transferred_cap=None sender_tid=2( |$)'
  local got_n got_at cap
  got_n="$(_u9_cre "$got_re")"
  got_at="$(_u9_line "$got_re")"
  cap="$(printf '%s\n' "$_U9_LOG" | rg -a -m1 -e "$got_re" 2>/dev/null | rg -o -m1 -e 'reply_cap=Some\([0-9]+\)' 2>/dev/null | tr -dc '0-9')"
  if [[ "$got_n" != "1" || -z "$cap" ]]; then
    echo "[error] $tag: PM receives tid 2's query exactly once, with a reply cap -- got ${got_n}"
    return 1
  fi

  # The transaction: the one arming of tid 2's terminal with a finite deadline names the record.
  local arm_re='^IPC_REPLY_TERMINAL_ARMED_SPLIT caller_tid=2 caller_asid=2 record_index=[0-9]+ record_generation=[0-9]+ .*finite_deadline=1 deadline_reserved=1 '
  local armed rec gen id
  armed="$(_u9_cre "$arm_re")"
  rec="$(_u9_field "$arm_re" 'record_index')"
  gen="$(_u9_field "$arm_re" 'record_generation')"
  if [[ "$armed" != "1" || -z "$rec" || -z "$gen" ]]; then
    echo "[error] $tag: the witnessed reply terminal (tid 2, finite deadline) is armed exactly once -- got ${armed}"
    return 1
  fi
  id="record_index=${rec} record_generation=${gen}"

  # Its deadline: armed exactly once on that exact record. Anchored on the arming existing, never
  # on the `deadline=` value (an absolute expiry tick that varies with when the caller blocked).
  local dl
  dl="$(_u9_cre "^IPC_REPLY_TIMEOUT_ARMED arch=aarch64 caller_tid=2 caller_asid=2 ${id} ")"
  if [[ "$dl" != "1" ]]; then
    echo "[error] $tag: a finite reply deadline must be armed exactly once on the witnessed record (${id}) -- armed=${dl}"
    bad=1
  fi

  # Exactly one route committed that exact record for tid 2, and it carried PM's cap.
  local queued_re direct_re queued_n direct_n route alias=""
  queued_re="^IPCCALL_QUEUED_SPLIT_OK tid=2 endpoint=[0-9]+ endpoint_generation=[0-9]+ reply_cap=[0-9]+ ${id} "
  direct_re="^IPCCALL_DIRECT_SPLIT_OK caller_tid=2 server_tid=3 endpoint=[0-9]+ endpoint_generation=[0-9]+ server_reply_cap=${cap} ${id} "
  queued_n="$(_u9_cre "$queued_re")"
  direct_n="$(_u9_cre "$direct_re")"
  if [[ "$queued_n" == "1" && "$direct_n" == "0" ]]; then
    route=queued
    alias="$(_u9_field "$queued_re" 'reply_cap')"
    # PM's one-shot is the cap the kernel bound to THIS record on the receive, minted once,
    # before the receive delivered it.
    local bound bound_any mint_n mint_at
    bound="$(_u9_c "IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index=${rec} reply_gen=${gen} cap=${cap}")"
    bound_any="$(_u9_cre "^IPC_RECV_REPLY_CAP_WAITER_CAP_SET reply_index=[0-9]+ reply_gen=[0-9]+ cap=${cap}( |\$)")"
    if [[ "$bound" != "1" || "$bound_any" != "1" ]]; then
      echo "[error] $tag: PM's cap ${cap} is bound to record (${rec}, ${gen}) exactly once and to no other -- exact=${bound} any=${bound_any}"
      return 1
    fi
    mint_n="$(_u9_c "IPC_REPLY_CAP_ONESHOT_OK receiver_tid=3 local_reply_cap=${cap}")"
    mint_at="$(_u9_line "IPC_REPLY_CAP_ONESHOT_OK receiver_tid=3 local_reply_cap=${cap}( |\$)")"
    if [[ "$mint_n" != "1" || -z "$mint_at" ]] || (( mint_at >= got_at )); then
      echo "[error] $tag: reply cap ${cap} must be minted for PM exactly once, before the receive -- mints=${mint_n} mint_line=${mint_at:-none} receive_line=${got_at}"
      return 1
    fi
  elif [[ "$direct_n" == "1" && "$queued_n" == "0" ]]; then
    route=direct
    # The direct commit itself hands PM the cap: it precedes the receive, and no receive mints
    # it a second time.
    local direct_at
    direct_at="$(_u9_line "$direct_re")"
    if (( direct_at >= got_at )); then
      echo "[error] $tag: the direct request must commit before PM's receive returns it -- commit_line=${direct_at} receive_line=${got_at}"
      return 1
    fi
    if [[ "$(_u9_c "IPC_REPLY_CAP_ONESHOT_OK receiver_tid=3 local_reply_cap=${cap}")" != "0" ]]; then
      echo "[error] $tag: the direct route's cap ${cap} was materialized a second time by a receive"
      return 1
    fi
  else
    echo "[error] $tag: tid 2's query must be attested by exactly one route for PM's cap ${cap} and ${id} -- queued=${queued_n} direct=${direct_n}"
    return 1
  fi

  # PM's cap resolves against this record and no other.
  local reply_resolve cap_resolves
  reply_resolve="$(_u9_cre "^IPC_REPLY_OBJECT_OK tid=3 cap=${cap} reply_index=${rec} generation=${gen}( |\$)")"
  cap_resolves="$(_u9_cre "^IPC_REPLY_OBJECT_OK tid=3 cap=${cap} reply_index=[0-9]+ generation=[0-9]+( |\$)")"
  if (( cap_resolves != reply_resolve )); then
    echo "[error] $tag: PM's cap ${cap} resolved against another record than (${rec}, ${gen}) -- exact=${reply_resolve} any=${cap_resolves}"
    return 1
  fi
  if (( reply_resolve > 1 )); then
    echo "[error] $tag: PM's cap ${cap} resolved more than once against (${rec}, ${gen}) -- ${reply_resolve}"
    return 1
  fi

  # ── the two winners, each discriminated by its own exact claim ──
  local reply_commit reply_resume replier_revoke replier_revoke_any caller_revoke caller_revoke_rec
  local want_exp="expected=Reply \\{ index: ${rec}, generation: ${gen} \\}"
  reply_commit="$(_u9_cre "^IPCREPLY_DIRECT_TERMINAL_CLAIM ${id} replier_tid=3 terminal=Reply resolution=commit settled=1")"
  reply_resume="$(_u9_c 'IPC_RECV_V2_META_BLOCKED_WAITER_OK tid=2 ')"
  replier_revoke="$(_u9_cre "IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=${cap} waiter_cap=[0-9]+ ${want_exp} ok=true")"
  replier_revoke_any="$(_u9_c "IPC_REPLY_REPLIER_CAP_FAST_REVOKE caller_tid=2 replier_tid=3 cap=${cap} ")"
  caller_revoke="$(_u9_cre "IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2 cap=${alias:-NONE} ${want_exp} ok=true")"
  caller_revoke_rec="$(_u9_cre "IPC_REPLY_CALLER_CAP_FAST_REVOKE caller_tid=2 cap=[0-9]+ ${want_exp}")"

  local timeout_ok timeout_commit timeout_wake aliases late_ok
  local late_refused=0
  timeout_ok="$(_u9_cre '^IPC_REPLY_TIMEOUT_OK arch=aarch64 terminal=Timeout timeout_result=TimedOut')"
  timeout_commit="$(_u9_c 'IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=aarch64 terminal=Timeout result=ok')"
  timeout_wake="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'caller_wakes')"
  aliases="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'reply_aliases_invalid')"
  late_ok="$(_u9_field '^IPC_REPLY_TIMEOUT_OK ' 'late_reply_successes')"
  # A late reply is refused by ONE of TWO owners, and the evaluator must know both or it will
  # report a refusal that happened as a refusal that did not:
  #   * the LEGACY refusal, raised inside the broad handler after `resolve_reply_index` finds the
  #     record no longer externally invokable -- `IPC_REPLY_FAIL ... err=WrongObject`;
  #   * the PRE-LOCK refusal that DIRECT3-CAP-FINAL §7 added, which answers the identical typed
  #     error at the verdict WITHOUT entering the broad dispatcher at all --
  #     `IPCREPLY_DIRECT_REFUSED_PRE_LOCK ... err=WrongObject`.
  # Both are scoped to the witnessed record (or PM's cap) and replier; neither is a wildcard.
  local late_refused_legacy late_refused_prelock prelock_inert
  late_refused_legacy="$(_u9_c "IPC_REPLY_FAIL tid=3 reply_cap=${cap} err=WrongObject")"
  late_refused_prelock="$(_u9_cre "^IPCREPLY_DIRECT_REFUSED_PRE_LOCK ${id} replier_tid=3 .*err=WrongObject")"
  # The pre-lock refusal must be INERT -- it is a refusal, not a partial reply.
  prelock_inert="$(_u9_cre "^IPCREPLY_DIRECT_REFUSED_PRE_LOCK ${id} replier_tid=3 terminal=Settled reply_copies=0 caller_wakes=0 mutations=0 err=WrongObject")"
  late_refused=$(( late_refused_legacy + late_refused_prelock ))
  : "${timeout_wake:=0}" ; : "${aliases:=0}" ; : "${late_ok:=0}"

  local reply_win=0 timeout_win=0
  [[ "$reply_resolve" == "1" ]] && reply_win=1
  [[ "$timeout_ok"    == "1" ]] && timeout_win=1

  # EXACTLY ONE WINNER. Zero is the genuinely lost reply; two is a double settlement.
  if (( reply_win + timeout_win != 1 )); then
    echo "[error] $tag: exactly one claimant must win -- reply_win=${reply_win} timeout_win=${timeout_win} (resolve=${reply_resolve} timeout_ok=${timeout_ok}, ${id})"
    return 1
  fi

  # EXACTLY ONE TERMINAL SETTLEMENT, and EXACTLY ONE CALLER WAKE, attributed to the winner.
  local settlements=$(( reply_commit + timeout_commit ))
  local wakes=$(( reply_resume + timeout_wake ))
  if (( settlements != 1 )); then
    echo "[error] $tag: the terminal must settle exactly once -- reply_commit=${reply_commit} timeout_commit=${timeout_commit} (${id})"
    bad=1
  fi
  if (( wakes != 1 )); then
    echo "[error] $tag: the caller must wake exactly once -- reply_resume=${reply_resume} timeout_wake=${timeout_wake}"
    bad=1
  fi
  # No revoke of PM's cap may name another record than the witnessed one.
  if (( replier_revoke_any != replier_revoke )); then
    echo "[error] $tag: the replier side of the one-shot is revoked once, against (${rec}, ${gen}) -- exact=${replier_revoke} any=${replier_revoke_any}"
    bad=1
  fi

  if (( reply_win )); then
    (( reply_commit == 1 ))   || { echo "[error] $tag: reply won but did not commit its terminal (commit=${reply_commit})"; bad=1; }
    (( reply_resume == 1 ))   || { echo "[error] $tag: reply won but the caller did not resume via the reply (resume=${reply_resume})"; bad=1; }
    (( timeout_wake == 0 ))   || { echo "[error] $tag: reply won yet the timeout also woke the caller (timeout_wake=${timeout_wake})"; bad=1; }
    (( late_ok == 0 ))        || { echo "[error] $tag: late_reply_successes must be 0 (got ${late_ok})"; bad=1; }
    (( replier_revoke == 1 )) || { echo "[error] $tag: the replier side of the one-shot is revoked once (got ${replier_revoke})"; bad=1; }
    if [[ "$route" == queued ]]; then
      (( caller_revoke == 1 )) || { echo "[error] $tag: the caller's alias ${alias} of the one-shot is revoked once, against (${rec}, ${gen}) (got ${caller_revoke})"; bad=1; }
    else
      (( caller_revoke_rec == 0 )) || { echo "[error] $tag: the direct route mints no caller alias, yet ${caller_revoke_rec} caller-side revoke(s) name (${rec}, ${gen})"; bad=1; }
    fi
    (( bad == 0 )) && echo "[ok] $tag: REPLY won the terminal of ${id} (${route} route, cap ${cap}) — commit=1 resume=1 one-shot revoked, no timeout win"
  else
    (( timeout_commit == 1 )) || { echo "[error] $tag: timeout won but did not commit its terminal (commit=${timeout_commit})"; bad=1; }
    (( timeout_wake == 1 ))   || { echo "[error] $tag: timeout won but did not wake the caller exactly once (caller_wakes=${timeout_wake})"; bad=1; }
    (( aliases == 1 ))        || { echo "[error] $tag: the reply aliases must be invalidated exactly once (got ${aliases})"; bad=1; }
    (( reply_resume == 0 ))   || { echo "[error] $tag: timeout owns the terminal, so the reply must NOT complete the caller (resume=${reply_resume})"; bad=1; }
    (( reply_commit == 0 ))   || { echo "[error] $tag: timeout owns the terminal, so no reply commit may exist (got ${reply_commit})"; bad=1; }
    (( late_ok == 0 ))        || { echo "[error] $tag: late_reply_successes must be 0 (got ${late_ok})"; bad=1; }
    (( late_refused >= 1 ))   || { echo "[error] $tag: a reply attempted after the timeout must be refused (legacy=${late_refused_legacy} prelock=${late_refused_prelock}, ${id})"; bad=1; }
    # Whichever owner refused, the refusal must have mutated nothing.
    (( late_refused_prelock == prelock_inert )) || { echo "[error] $tag: a pre-lock refusal was not inert (refusals=${late_refused_prelock} inert=${prelock_inert})"; bad=1; }
    # The one-shot must not be half-revoked behind a timeout win.
    (( replier_revoke_any == 0 )) || { echo "[error] $tag: timeout won yet the replier one-shot slot was revoked by a reply (got ${replier_revoke_any})"; bad=1; }
    (( bad == 0 )) && echo "[ok] $tag: TIMEOUT won the terminal of ${id} (${route} route, cap ${cap}) — commit=1 caller_wakes=1 aliases_invalid=1 late reply refused (legacy=${late_refused_legacy} prelock=${late_refused_prelock}, inert), no reply completion"
  fi

  # Neither outcome may leak, on any path.
  local leak_cap leak_link leak_rec
  leak_cap="$(_u9_c 'IPC_REPLY_FAST_REVOKE_FAIL')"
  leak_link="$(_u9_c 'IPC_SERVER_REPLY_LINK_REGISTER_FAIL')"
  leak_rec="$(_u9_c 'err=CapabilityFull')"
  if (( leak_cap + leak_link + leak_rec != 0 )); then
    echo "[error] $tag: leak on the witnessed path -- cap=${leak_cap} link=${leak_link} record=${leak_rec}"
    bad=1
  fi
  return $bad
}

# u9rx4_selftest FIXTURE_DIR — run the evaluator over every case of the fixture corpus
# (`cases.tsv`: name, accept|reject, reason). A reject must fail for its named reason, so a case
# cannot pass by tripping an unrelated check. Prints one summary line; returns non-zero on any
# disagreement.
u9rx4_selftest() {
  local dir="$1" name expect reason log out got acc=0 rej=0 fail=0
  if [[ ! -s "$dir/cases.tsv" ]]; then
    echo "[error] U9-RX4 evaluator self-test: no fixture corpus at $dir"
    return 1
  fi
  while IFS=$'\t' read -r name expect reason; do
    log="$(tr '\r' '\n' <"$dir/$name.log")"
    if out="$(u9rx4_eval_terminal "$log" SELFTEST 2>&1)"; then got=accept; else got=reject; fi
    if [[ "$got" != "$expect" ]]; then
      echo "[error] U9-RX4 evaluator self-test: '$name' expected $expect, got $got"
      printf '%s\n' "$out" | sed 's/^/    /'
      fail=1
    elif [[ "$expect" == reject && -n "$reason" && "$out" != *"$reason"* ]]; then
      echo "[error] U9-RX4 evaluator self-test: '$name' was rejected, but not because '$reason'"
      printf '%s\n' "$out" | sed 's/^/    /'
      fail=1
    fi
    if [[ "$expect" == accept ]]; then acc=$((acc + 1)); else rej=$((rej + 1)); fi
  done <"$dir/cases.tsv"
  if (( fail )); then
    echo "[error] U9-RX4 terminal-outcome evaluator self-test FAILED"
    return 1
  fi
  echo "[ok] U9-RX4: terminal-outcome evaluator self-test passed (${acc} accepted, ${rej} rejected)"
}
