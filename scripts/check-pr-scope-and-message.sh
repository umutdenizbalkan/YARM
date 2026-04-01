#!/usr/bin/env bash
set -euo pipefail

# Review-followup CI gate:
# 1) reject placeholder/WIP commit messages
# 2) reject overscoped architecture PR slices by default
#
# Usage:
#   BASE_REF=<git rev> HEAD_REF=<git rev> ./scripts/check-pr-scope-and-message.sh
# Defaults:
#   BASE_REF=HEAD~1
#   HEAD_REF=HEAD
# Escape hatch:
#   ALLOW_OVERSCOPE=1

BASE_REF="${BASE_REF:-HEAD~1}"
HEAD_REF="${HEAD_REF:-HEAD}"

range="${BASE_REF}..${HEAD_REF}"

if ! git rev-parse --verify "${BASE_REF}" >/dev/null 2>&1; then
  echo "[gate] base ref '${BASE_REF}' does not exist"
  exit 2
fi

messages="$(git log --format=%s "${range}")"
if [[ -z "${messages}" ]]; then
  echo "[gate] no commits found in ${range}"
  exit 2
fi

if echo "${messages}" | rg -i "(^|\\b)(wip|placeholder|tmp|todo|fixup!|squash!)(\\b|$)" >/dev/null; then
  echo "[gate] commit message contains placeholder/wip markers"
  exit 1
fi

mapfile -t changed_files < <(git diff --name-only "${BASE_REF}" "${HEAD_REF}")
if [[ "${#changed_files[@]}" -eq 0 ]]; then
  echo "[gate] no changed files in ${range}"
  exit 2
fi

touches_arch=0
touches_kernel=0
touches_services=0

for f in "${changed_files[@]}"; do
  [[ "${f}" == src/arch/* ]] && touches_arch=1
  [[ "${f}" == src/kernel/* ]] && touches_kernel=1
  [[ "${f}" == src/services/* ]] && touches_services=1
done

if [[ "${ALLOW_OVERSCOPE:-0}" != "1" && "${touches_arch}" -eq 1 ]]; then
  scope_count=$((touches_arch + touches_kernel + touches_services))
  if [[ "${scope_count}" -gt 2 ]]; then
    echo "[gate] architecture change appears overscoped (arch + kernel + services touched)"
    echo "[gate] split into review slices (e.g., PR A/B/C style) or run with ALLOW_OVERSCOPE=1"
    exit 1
  fi
fi

echo "[ok] PR scope/message gate passed for ${range}"
