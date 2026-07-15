#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198C3 — REPLY-CAP DIRECT NEGATIVE + ROLLBACK SEAL.
#
# Proves the reply-cap DIRECT delivery path fails CLOSED across the required negative
# matrix, driving the ACTUAL production producer/executor + reply-cap lifecycle helpers
# (produce_blocked_waiter_reply_cap_delivery + drain_dispatch_post_work; ipc_reply /
# reply-record teardown). Every case asserts its rollback invariant, so the seal's
# leaked_reply_caps / leaked_reply_objects / duplicate_wakes / duplicate_replies are 0
# by construction (the seal fails unless every case passes).
#
# Cases (hosted, production-path):
#   invalid source cap                     stage198c1_invalid_source_handle_rejected_no_stash
#   stale source generation                stage198c1_stale_source_generation_rejected_no_stash
#   non-Reply source                       stage198c1_non_reply_source_rejected_by_reply_producer_no_stash
#   invalid receiver payload dest          stage198c1_invalid_receiver_payload_dest_rejected_no_stash
#   invalid receiver metadata dest         stage198c3_invalid_receiver_meta_dest_rejected_no_stash
#   provisional-cap rollback (copy fault)  stage198c1_provisional_cap_rolled_back_on_executor_copy_fault
#   Phase-B' stale-record rollback         stage188d_stale_record_rolls_back_no_wake
#   duplicate transfer attempt             stage198c3_duplicate_transfer_envelope_rejected_no_double_mint
#   one-shot record arbiter / dup invoke   stage198c1_delegation_sender_cap_present_record_is_one_shot_arbiter
#   caller exit revoke                     reply_caps_are_revoked_when_caller_exits
#   caller reap (marked dead) revoke       reply_caps_are_revoked_when_caller_marked_dead
#   caller generation replacement          old_reply_cap_replay_is_rejected_after_restart_and_remint
#   reused-TID stale reply rejected        duplicated_stale_reply_cap_is_rejected_after_caller_restart
#   rollback clears slot + waiter cap      stage20_rollback_materialized_reply_cap_clears_slot_and_waiter_id
#   record slot cleared on revoke          revoke_reply_cap_record_clears_global_slot
#   receiver cnode teardown cleanup        stage24_stale_reply_cap_cannot_be_reused_after_cnode_teardown
#   server exit holding delegated cap      stage25c_reply_cap_cannot_be_reused_after_replier_teardown
#   unbound-responder rejection            reply_cap_rejects_use_from_unbound_responder_task
#
# Emits on success:
#   SECOND_COHORT_REPLY_CAP_DIRECT_NEGATIVE_SEAL cases=<n> leaked_reply_caps=0 \
#       leaked_reply_objects=0 duplicate_wakes=0 duplicate_replies=0 result=ok
set -uo pipefail
cd "$(dirname "$0")/.."

note() { echo "[neg-seal] $*"; }

CASES=(
  stage198c1_invalid_source_handle_rejected_no_stash
  stage198c1_stale_source_generation_rejected_no_stash
  stage198c1_non_reply_source_rejected_by_reply_producer_no_stash
  stage198c1_invalid_receiver_payload_dest_rejected_no_stash
  stage198c3_invalid_receiver_meta_dest_rejected_no_stash
  stage198c1_provisional_cap_rolled_back_on_executor_copy_fault
  stage188d_stale_record_rolls_back_no_wake
  stage198c3_duplicate_transfer_envelope_rejected_no_double_mint
  stage198c1_delegation_sender_cap_present_record_is_one_shot_arbiter
  reply_caps_are_revoked_when_caller_exits
  reply_caps_are_revoked_when_caller_marked_dead
  old_reply_cap_replay_is_rejected_after_restart_and_remint
  duplicated_stale_reply_cap_is_rejected_after_caller_restart
  stage20_rollback_materialized_reply_cap_clears_slot_and_waiter_id
  revoke_reply_cap_record_clears_global_slot
  stage24_stale_reply_cap_cannot_be_reused_after_cnode_teardown
  stage25c_reply_cap_cannot_be_reused_after_replier_teardown
  reply_cap_rejects_use_from_unbound_responder_task
)
expected="${#CASES[@]}"

log="${TMPDIR:-/tmp}/reply-cap-neg-seal.$$.log"
note "running ${expected} reply-cap direct negative/rollback cases"
# Multiple test-name filters go to libtest (after `--`); cargo takes one positional.
if ! cargo test --features hosted-dev --lib -- --test-threads=1 "${CASES[@]}" >"$log" 2>&1; then
  echo "[neg-seal][fail] reply-cap negative cases did not all pass:"
  grep -E "FAILED|error\[|panicked|test result" "$log" | head
  echo "SECOND_COHORT_REPLY_CAP_DIRECT_NEGATIVE_SEAL cases=${expected} result=fail"
  rm -f "$log"
  exit 1
fi
passed="$(grep -oE 'result: ok\. [0-9]+ passed' "$log" | grep -oE '[0-9]+' | head -1)"
rm -f "$log"
if [[ "${passed:-0}" -ne "$expected" ]]; then
  echo "[neg-seal][fail] expected ${expected} passing cases, got ${passed}"
  echo "SECOND_COHORT_REPLY_CAP_DIRECT_NEGATIVE_SEAL cases=${expected} passed=${passed:-0} result=fail"
  exit 1
fi
note "${passed}/${expected} reply-cap negative cases passed"
echo "SECOND_COHORT_REPLY_CAP_DIRECT_NEGATIVE_SEAL cases=${passed} leaked_reply_caps=0 leaked_reply_objects=0 duplicate_wakes=0 duplicate_replies=0 result=ok"
