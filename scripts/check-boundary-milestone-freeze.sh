#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

if ! rg -n "## Boundary milestone status" MICROKERNEL_BOUNDARY.md >/dev/null; then
  echo "[fail] boundary milestone status section missing in MICROKERNEL_BOUNDARY.md"
  exit 1
fi

if ! rg -n "✅ \*\*COMPLETE\*\*" MICROKERNEL_BOUNDARY.md >/dev/null; then
  echo "[fail] boundary milestone completion marker missing in MICROKERNEL_BOUNDARY.md"
  exit 1
fi

if ! rg -n "PR-BND-6 pass C landed" KERNEL_STATUS.md >/dev/null; then
  echo "[fail] KERNEL_STATUS.md must record PR-BND-6 pass C landed"
  exit 1
fi

echo "[ok] boundary milestone freeze checks passed"
