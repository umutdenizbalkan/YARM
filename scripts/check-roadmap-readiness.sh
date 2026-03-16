#!/usr/bin/env bash
set -euo pipefail

bad=0

if ! rg -n "## Architecture follow-up status \(frozen\)" SERVER_ROADMAP.md >/dev/null; then
  echo "[fail] roadmap must keep architecture follow-up section marked frozen"
  bad=1
fi

for f in PHASE2_DRIVER_CONTRACT.md PHASE3_NETWORK_CONTRACT.md; do
  if [[ ! -f "$f" ]]; then
    echo "[fail] missing required phase contract: $f"
    bad=1
  fi
done

if ! rg -n "phase2-driver-gates|phase3-network-gates|phase4-ui-gates" .github/workflows/compat-gates.yml >/dev/null; then
  echo "[fail] compat-gates must include phase2/phase3/phase4 jobs"
  bad=1
fi

if [[ "$bad" -ne 0 ]]; then
  exit 1
fi

echo "[ok] roadmap readiness checks passed"
