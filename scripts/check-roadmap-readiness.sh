#!/usr/bin/env bash
set -euo pipefail

bad=0

if ! rg -n "## Architecture follow-up status \(frozen\)" SERVER_ROADMAP.md >/dev/null; then
  echo "[fail] roadmap must keep architecture follow-up section marked frozen"
  bad=1
fi

for f in PHASE2_DRIVER_CONTRACT.md PHASE3_NETWORK_CONTRACT.md PHASE4_UI_CONTRACT.md; do
  if [[ ! -f "$f" ]]; then
    echo "[fail] missing required phase contract: $f"
    bad=1
  fi
done

if ! rg -n "phase2-driver-gates|phase3-network-gates|phase4-ui-gates|phase4-ui-smoke-marker" .github/workflows/compat-gates.yml >/dev/null; then
  echo "[fail] compat-gates must include phase2/phase3/phase4 jobs"
  bad=1
fi

# semantic readiness: if a phase says all target services are implemented, it must have gate wiring + contract docs
if rg -n "## Phase 2 — Device Driver Servers" SERVER_ROADMAP.md >/dev/null; then
  if ! rg -n "delegation gate: .*wired to compat-gates workflow|fault gate: .*wired to compat-gates workflow" SERVER_ROADMAP.md >/dev/null; then
    echo "[fail] phase2 readiness text must declare gate wiring"
    bad=1
  fi
fi

if rg -n "## Architecture follow-up addenda" SERVER_ROADMAP.md >/dev/null; then
  # addenda lines should be dated bullets like: - YYYY-MM-DD: ...
  if ! rg -n "^- [0-9]{4}-[0-9]{2}-[0-9]{2}:" SERVER_ROADMAP.md >/dev/null; then
    echo "[fail] architecture addenda must include at least one dated entry (- YYYY-MM-DD: ...)"
    bad=1
  fi
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] roadmap readiness checks passed"
