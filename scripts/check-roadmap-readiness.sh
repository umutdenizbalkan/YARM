#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Roadmap / phase readiness gate.
#
# After Pass 4 documentation consolidation (2026-06-15), the per-phase contract
# docs (PHASE2/3/4_*) and the readiness matrix were merged into the canonical
# doc/PHASE_GATES.md. The kernel-status snapshot moved to doc/STATUS.md.
# This script now greps the canonical doc(s); the literal section headings and
# CI tokens are preserved verbatim across the move so the gate remains intact.

PHASE_GATES_DOC=${PHASE_GATES_DOC:-doc/PHASE_GATES.md}

bad=0

if [[ ! -f "$PHASE_GATES_DOC" ]]; then
  echo "[fail] missing required phase-gates doc: $PHASE_GATES_DOC"
  exit 1
fi

if ! rg -n "## Architecture follow-up status \(frozen\)" "$PHASE_GATES_DOC" >/dev/null; then
  echo "[fail] phase-gates doc must keep architecture follow-up section marked frozen"
  bad=1
fi

if ! rg -n "phase2-driver-gates|phase3-network-gates|phase4-ui-gates|phase4-ui-smoke-marker|phase5-boundary-gates" .github/workflows/compat-gates.yml >/dev/null; then
  echo "[fail] compat-gates must include phase2/phase3/phase4/phase5 jobs"
  bad=1
fi

# semantic readiness: if a phase says all target services are implemented, it must have gate wiring + contract docs
if rg -n "## Phase 2 — Device Driver Servers" "$PHASE_GATES_DOC" >/dev/null; then
  if ! rg -n "delegation gate: .*wired to compat-gates workflow|fault gate: .*wired to compat-gates workflow" "$PHASE_GATES_DOC" >/dev/null; then
    echo "[fail] phase2 readiness text must declare gate wiring"
    bad=1
  fi
fi

if rg -n "## Architecture follow-up addenda" "$PHASE_GATES_DOC" >/dev/null; then
  # addenda lines should be dated bullets like: - YYYY-MM-DD: ...
  if ! rg -n "^- [0-9]{4}-[0-9]{2}-[0-9]{2}:" "$PHASE_GATES_DOC" >/dev/null; then
    echo "[fail] architecture addenda must include at least one dated entry (- YYYY-MM-DD: ...)"
    bad=1
  fi
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

# matrix/workflow synchronization checks
for token in phase2-driver-gates phase3-network-gates phase4-ui-gates phase4-ui-smoke-marker phase5-boundary-gates; do
  if ! rg -n "$token" "$PHASE_GATES_DOC" >/dev/null; then
    echo "[fail] phase-gates doc missing CI token: $token"
    bad=1
  fi
  if ! rg -n "$token" .github/workflows/compat-gates.yml .github/workflows/core-qemu-smoke.yml >/dev/null; then
    echo "[fail] workflows missing CI token from matrix: $token"
    bad=1
  fi
done

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] roadmap readiness checks passed"
