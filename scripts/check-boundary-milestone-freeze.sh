#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

BOUNDARY_DOC=${BOUNDARY_DOC:-doc/MICROKERNEL_BOUNDARY.md}
KERNEL_STATUS_DOC=${KERNEL_STATUS_DOC:-doc/KERNEL_STATUS.md}

if [[ ! -f "$BOUNDARY_DOC" ]]; then
  echo "[fail] boundary milestone document missing: $BOUNDARY_DOC"
  exit 1
fi

if [[ ! -f "$KERNEL_STATUS_DOC" ]]; then
  echo "[fail] kernel status document missing: $KERNEL_STATUS_DOC"
  exit 1
fi

if ! rg -n "## Boundary milestone status" "$BOUNDARY_DOC" >/dev/null; then
  echo "[fail] boundary milestone status section missing in $BOUNDARY_DOC"
  exit 1
fi

if ! rg -n "✅ \*\*COMPLETE\*\*" "$BOUNDARY_DOC" >/dev/null; then
  echo "[fail] boundary milestone completion marker missing in $BOUNDARY_DOC"
  exit 1
fi

if ! rg -n "PR-BND-6 pass C landed" "$KERNEL_STATUS_DOC" >/dev/null; then
  echo "[fail] $KERNEL_STATUS_DOC must record PR-BND-6 pass C landed"
  exit 1
fi

echo "[ok] boundary milestone freeze checks passed"
