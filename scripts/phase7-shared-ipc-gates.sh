#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Phase 7 shared-memory IPC hardening gates

required_docs=(
  "IPC_SHARED_MEMORY_FASTPATH_PLAN.md"
  "SHARED_IPC_MIGRATION_GUIDE.md"
  "SHARED_IPC_THROUGHPUT_GUIDE.md"
)
for doc in "${required_docs[@]}"; do
  [[ -f "$doc" ]] || { echo "[fail] missing required doc: $doc"; exit 1; }
done

if ! rg -n "Phase 7" IPC_SHARED_MEMORY_FASTPATH_PLAN.md >/dev/null; then
  echo "[fail] Phase 7 section missing in IPC_SHARED_MEMORY_FASTPATH_PLAN.md"
  exit 1
fi

if ! rg -n "shared_mem_canary_map_release_parity_under_repeated_load" src/kernel/syscall.rs >/dev/null; then
  echo "[fail] Phase 7 runtime canary test not found"
  exit 1
fi

if ! rg -n "syscall_recv_shared_mem_requires_nonzero_map_target" src/kernel/syscall.rs >/dev/null; then
  echo "[fail] migration enforcement test not found"
  exit 1
fi

cargo test -q shared_mem_canary_map_release_parity_under_repeated_load
cargo test -q syscall_recv_shared_mem_requires_nonzero_map_target

echo "[ok] phase7 shared IPC gates passed"
