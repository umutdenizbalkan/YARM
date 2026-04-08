#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

bad=0

compat=".github/workflows/compat-gates.yml"
smoke=".github/workflows/core-qemu-smoke.yml"

for wf in "$compat" "$smoke"; do
  if [[ ! -f "$wf" ]]; then
    echo "[fail] missing workflow: $wf"
    bad=1
  fi
done

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

if ! rg -n "pull_request:" "$compat" >/dev/null; then
  echo "[fail] compat-gates workflow must run on pull_request"
  bad=1
fi
if ! rg -n "core-profile:" "$compat" >/dev/null; then
  echo "[fail] compat-gates workflow must include core-profile job"
  bad=1
fi
if ! rg -n "posix-compat-profile:" "$compat" >/dev/null; then
  echo "[fail] compat-gates workflow must include posix-compat-profile job"
  bad=1
fi
if ! rg -n "phase5-boundary-gates:" "$compat" >/dev/null; then
  echo "[fail] compat-gates workflow must include phase5-boundary-gates job"
  bad=1
fi
if ! rg -n "tid-allocation-policy-gates:" "$compat" >/dev/null; then
  echo "[fail] compat-gates workflow must include tid-allocation-policy-gates job"
  bad=1
fi

if ! rg -n "pull_request:" "$smoke" >/dev/null; then
  echo "[fail] core-qemu-smoke workflow must run on pull_request"
  bad=1
fi

smoke_jobs=0
for job in x86_64-core-smoke aarch64-core-smoke riscv64-core-smoke; do
  if rg -n "^  ${job}:" "$smoke" >/dev/null; then
    smoke_jobs=$((smoke_jobs + 1))
  fi
done
if [[ "$smoke_jobs" -lt 1 ]]; then
  echo "[fail] core-qemu-smoke must include at least one architecture smoke job"
  bad=1
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] CI workflow enforcement checks passed"
