#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B1 Part B / Stage 198F — COMBINED RETIREMENT SEAL (Model 1: serialized master).
#
# Runs the functional cohort seals STRICTLY SEQUENTIALLY — first-cohort
# (12 cells), second-cohort plain (6 cells), second-cohort ordinary-cap (6
# cells), second-cohort reply-cap-direct (3 cells), the shared-region DIRECT
# matrix (3 cells), and the supervisor crash-restart baseline — never
# concurrently. Serialization is the isolation model: only ONE QEMU runs at a
# time, so there is no CPU/memory starvation (the root cause of the Stage 198B
# AArch64-enqueue 5/6 partial and the first-cohort exit-124 timeout) and no
# shared log/artifact/socket contention. Each seal gets a unique per-run LOGDIR
# so repeated runs cannot cross-contaminate.
#
# Stage 198F adds exact-commit provenance (clean tree + fresh-from-HEAD artifacts
# + a current-run manifest), the shared-region DIRECT matrix cohort, and the
# top-level 30-cell STAGE_198F_COMPLETE_RETIREMENT_SEAL computed from an EXPLICIT
# class manifest (not broad substring counts). This is a HOST-SIDE seal — it is
# NEVER emitted as a kernel DebugLog marker literal.
#
# Prints one line per seal, the legacy COMBINED_RETIREMENT_SEAL verdict, and the
# Stage 198F header + detail seal lines.
set -uo pipefail
cd "$(dirname "$0")/.."

RUN_ID="${RUN_ID:-$(date +%s)-$$}"
LOGROOT="${LOGROOT:-/tmp/combined-retirement-seal/${RUN_ID}}"
mkdir -p "$LOGROOT"
note() { echo "[combined-seal] $*"; }

overall=0      # any cell failed (gates COMBINED_RETIREMENT_SEAL)
func_fail=0    # any of the FUNCTIONAL cohorts failed (gates SECOND_COHORT_PROGRESS)
s198f_fail=0   # any Stage 198F provenance/cohort check failed (gates the 30-cell seal)

# ── Stage 198F: exact-commit provenance ──────────────────────────────────────
SEAL_COMMIT="$(git rev-parse HEAD)"
BUILD_START_EPOCH="$(date +%s)"
MANIFEST="$LOGROOT/stage198f-manifest.txt"
SKIP_ARTIFACT_BUILD="${SKIP_ARTIFACT_BUILD:-0}"
declare -A NORM_ART=(
  [x86_64]=build-x86_64/kernel_boot.elf
  [aarch64]=build-aarch64/yarm-aarch64.bin
  [riscv64]=build-riscv64/yarm-riscv64.bin
)
declare -A NORM_SHA=()

fail198f() { echo "[combined-seal][198f-fail] $*"; s198f_fail=1; overall=1; }

# The acceptance seal requires a CLEAN working tree so every artifact is built
# from exactly SEAL_COMMIT (build-*/ + target/ are gitignored, so builds do not
# dirty the tree). A dirty tree fails closed BEFORE any build/boot.
if [[ -n "$(git status --porcelain)" ]]; then
  note "DIRTY TREE — refusing to seal at $SEAL_COMMIT"
  git status --porcelain | head
  echo "STAGE_198F_COMPLETE_RETIREMENT_SEAL result=fail reason=dirty_tree commit=$SEAL_COMMIT"
  exit 1
fi
note "seal commit=$SEAL_COMMIT run_id=$RUN_ID build_start=$BUILD_START_EPOCH logroot=$LOGROOT"
{
  echo "# Stage 198F complete-retirement-seal manifest"
  echo "seal_commit=$SEAL_COMMIT"
  echo "run_id=$RUN_ID"
  echo "build_start_epoch=$BUILD_START_EPOCH"
  echo "logroot=$LOGROOT"
  echo "# columns: arch cohort artifact_path sha256 size mtime_epoch log_path seal_result"
} > "$MANIFEST"

manifest_row() { # arch cohort artifact_path log result
  local arch="$1" cohort="$2" art="$3" log="$4" res="$5"
  local sha="-" size="-" mt="-"
  if [[ -f "$art" ]]; then
    sha="$(sha256sum "$art" | cut -d' ' -f1)"
    size="$(stat -c%s "$art")"
    mt="$(stat -c%Y "$art")"
  fi
  echo "$arch $cohort $art $sha $size $mt $log $res" >> "$MANIFEST"
}

# Fail closed if an artifact predates the run (inherited/stale, not built from HEAD).
require_fresh() { # arch artifact
  local arch="$1" art="$2"
  if [[ ! -f "$art" ]]; then fail198f "missing artifact ($arch): $art"; return 1; fi
  local mt; mt="$(stat -c%Y "$art")"
  if [[ "$mt" -lt "$BUILD_START_EPOCH" ]]; then
    fail198f "STALE artifact ($arch): $art mtime=$mt < build_start=$BUILD_START_EPOCH"; return 1
  fi
  return 0
}

# ── Stage 198F: fresh NORMAL 3-arch artifacts (marker-clean base for the first +
# non-shared second cohorts). The shared-region matrix builds its OWN armed
# artifacts LAST (it overwrites build-<arch>/), so these run before it. ──
if [[ "$SKIP_ARTIFACT_BUILD" != "1" ]]; then
  for arch in x86_64 aarch64 riscv64; do
    note "── building fresh NORMAL $arch artifacts (from $SEAL_COMMIT) ──"
    if ! BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
        bash "scripts/build-qemu-${arch}-artifacts.sh" > "$LOGROOT/build_${arch}.log" 2>&1; then
      fail198f "NORMAL artifact build failed ($arch) — see $LOGROOT/build_${arch}.log"
    fi
  done
fi
for arch in x86_64 aarch64 riscv64; do
  if require_fresh "$arch" "${NORM_ART[$arch]}"; then
    NORM_SHA[$arch]="$(sha256sum "${NORM_ART[$arch]}" | cut -d' ' -f1)"
  fi
done

run_seal() { # run_seal <name> <script> <expected-final-marker> [class] [extra-env...]
  local name="$1" script="$2" expect="$3" class="${4:-func}"
  shift 4 2>/dev/null || shift $#
  local extra_env=("$@")
  local logdir="$LOGROOT/$name"
  local log="$LOGROOT/${name}.log"
  mkdir -p "$logdir"
  note "── running $name (serial; LOGDIR=$logdir) ──"
  local rc=0
  # ORACLE_RUN_ID keys the oracle wrapper's scratch dir uniquely per seal+run.
  # `env "${extra_env[@]}" bash …` applies any per-cell overrides (e.g. a larger
  # crash-restart wall-clock budget); an empty array degrades to `env bash …`.
  LOGDIR="$logdir" ORACLE_RUN_ID="${RUN_ID}-${name}" QEMU_SMOKE_STRICT=1 \
    env "${extra_env[@]}" bash "$script" > "$log" 2>&1 || rc=$?
  # The expected seal must come from THIS run's captured stdout ($log lives under
  # the per-run LOGROOT), never a pre-existing/inherited log directory.
  if grep -qa -- "$expect" "$log"; then
    note "$name: OK ($expect)"
    echo "COMBINED_SEAL_CELL name=$name result=ok"
    SEAL_RESULT[$name]=ok
  else
    note "$name: FAIL (rc=$rc; expected '$expect') — see $log"
    echo "COMBINED_SEAL_CELL name=$name result=fail rc=$rc"
    overall=1
    SEAL_RESULT[$name]=fail
    [[ "$class" == "func" ]] && func_fail=1
    [[ "$class" == "func" ]] && s198f_fail=1
  fi
  # A timeout manifests as exit 124 from the inner `timeout` wrapper; surface it.
  if [[ "$rc" -eq 124 ]]; then
    note "$name: TIMEOUT (exit 124)"
    echo "COMBINED_SEAL_CELL name=$name result=timeout"
    overall=1
    SEAL_RESULT[$name]=timeout
    [[ "$class" == "func" ]] && func_fail=1
    [[ "$class" == "func" ]] && s198f_fail=1
  fi
  SEAL_LOG[$name]="$log"
}
declare -A SEAL_RESULT=() SEAL_LOG=()

# The crash-restart baseline drives the LONGEST single QEMU run in the battery
# (~414k serial lines to reach DEGRADED_TERMINAL_APPLY_OK). Run it FIRST, while
# the container still has full CPU burst headroom, and give it a generous
# wall-clock budget: late in a long serialized run the host can throttle to
# baseline and the default 240s truncates the chain mid-restart (a wall-clock
# artifact, not a missing transition).
run_seal crash_restart  scripts/qemu-supervisor-crash-restart-smoke.sh \
  "SUPERVISOR_CRASH_RESTART_BASELINE .*result=ok" baseline TIMEOUT_SECS=600

run_seal first_cohort   scripts/qemu-first-cohort-retirement-seal.sh \
  "FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4 result=ok" func
run_seal plain          scripts/qemu-second-cohort-plain-seal.sh \
  "SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok" func
run_seal ordinary_cap   scripts/qemu-second-cohort-ordinary-cap-seal.sh \
  "SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok" func
run_seal reply_cap_direct scripts/qemu-second-cohort-reply-cap-direct-seal.sh \
  "SECOND_COHORT_REPLY_CAP_DIRECT_SEAL arches=3 classes=1 live_cells=3 result=ok" func

# The 27 non-shared cohorts booted the NORMAL artifacts; confirm they were NOT
# mutated mid-run (the recorded post-build hash must still match) BEFORE the
# shared-region matrix overwrites build-<arch>/ with feature-armed kernels.
for arch in x86_64 aarch64 riscv64; do
  art="${NORM_ART[$arch]}"
  if [[ -n "${NORM_SHA[$arch]:-}" && -f "$art" ]]; then
    now="$(sha256sum "$art" | cut -d' ' -f1)"
    if [[ "$now" != "${NORM_SHA[$arch]}" ]]; then
      fail198f "NORMAL artifact hash changed mid-run ($arch): $art"
    fi
    manifest_row "$arch" first_second_normal "$art" "$LOGROOT" "sha=${NORM_SHA[$arch]}"
  fi
done

# Stage 198F: the shared-region DIRECT matrix (3 cells). Runs LAST because its
# per-arch smokes rebuild feature-armed artifacts over build-<arch>/. The matrix
# runner already fails closed on missing/duplicate cells, fuse, enqueue, dup wake.
run_seal shared_region_direct scripts/qemu-shared-region-direct-matrix-seal.sh \
  "SECOND_COHORT_SHARED_REGION_DIRECT_MATRIX_SEAL arches=3 classes=1 live_cells=3 fuse_trips=0 duplicate_wakes=0 result=ok" func

# Record + freshness-check the ARMED shared-region artifacts (rebuilt by the matrix).
for arch in x86_64 aarch64 riscv64; do
  art="${NORM_ART[$arch]}"
  require_fresh "$arch" "$art" || true
  manifest_row "$arch" shared_region_direct "$art" "${SEAL_LOG[shared_region_direct]:-$LOGROOT}" "${SEAL_RESULT[shared_region_direct]:-unknown}"
done

# Second-cohort progress reflects ONLY the four legacy functional cohorts; it is
# not gated on the crash-restart baseline or the shared-region matrix cell.
if [[ "$func_fail" -eq 0 ]]; then
  echo "SECOND_COHORT_PROGRESS first=12 plain=6 ordinary_cap=6 reply_cap_direct=3 result=ok"
fi

if [[ "$overall" -eq 0 ]]; then
  echo "COMBINED_RETIREMENT_SEAL first=12 plain=6 ordinary_cap=6 reply_cap_direct=3 crash_restart=ok result=ok"
else
  echo "COMBINED_RETIREMENT_SEAL result=fail"
fi

# ── Stage 198F: 30-cell total from an EXPLICIT class manifest (not substring counts) ──
# Each supported cohort seal encodes its cell count in the exact marker verified above.
# The tuple list is the authoritative class manifest: (cohort-name, cells). The first
# cohort is 4 classes × 3 arches = 12; the six IpcSend classes are grouped by their
# cohort seals (plain=2 classes, ordinary_cap=2, reply_cap_direct=1, shared_region=1)
# for 6 classes × 3 arches = 18. Total supported live cells = 30.
declare -A COHORT_CELLS=(
  [first_cohort]=12
  [plain]=6
  [ordinary_cap]=6
  [reply_cap_direct]=3
  [shared_region_direct]=3
)
first_cohort_cells=0
ipc_send_cells=0
all_ok=1
for name in first_cohort plain ordinary_cap reply_cap_direct shared_region_direct; do
  if [[ "${SEAL_RESULT[$name]:-}" != "ok" ]]; then
    all_ok=0
    note "198F: cohort $name did not pass in THIS run (result=${SEAL_RESULT[$name]:-absent})"
    continue
  fi
  if [[ "$name" == "first_cohort" ]]; then
    first_cohort_cells=$((first_cohort_cells + COHORT_CELLS[$name]))
  else
    ipc_send_cells=$((ipc_send_cells + COHORT_CELLS[$name]))
  fi
done
total_cells=$((first_cohort_cells + ipc_send_cells))

# Aggregate fuse / duplicate-wake accounting across every captured cohort log
# (fail closed on ANY fuse trip or a duplicate shared-region post-work publication).
fuse_total=0
dupwake_total=0
if compgen -G "$LOGROOT/*.log" > /dev/null; then
  fuse_total=$(grep -rslaF "SHARED_REGION_CANCEL_FUSE_SET" "$LOGROOT"/*.log 2>/dev/null | wc -l | tr -d '[:space:]')
fi
# A duplicate wake would show a SECOND shared-region post-work completion in the
# shared-region cohort log; the matrix runner already gates duplicate_wakes=0.
if [[ -f "${SEAL_LOG[shared_region_direct]:-/nonexistent}" ]]; then
  if grep -qaF "duplicate_wakes=0 result=ok" "${SEAL_LOG[shared_region_direct]}"; then
    dupwake_total=0
  else
    dupwake_total=1
  fi
fi

# Tree must be UNCHANGED (same HEAD, still clean) across the whole run.
END_COMMIT="$(git rev-parse HEAD)"
if [[ "$END_COMMIT" != "$SEAL_COMMIT" ]]; then
  fail198f "HEAD changed during run: $SEAL_COMMIT -> $END_COMMIT"
fi
if [[ -n "$(git status --porcelain)" ]]; then
  fail198f "tree became dirty during run"
fi

{
  echo "# totals"
  echo "first_cohort_cells=$first_cohort_cells ipc_send_cells=$ipc_send_cells total_cells=$total_cells"
  echo "fuse_total=$fuse_total dupwake_total=$dupwake_total end_commit=$END_COMMIT s198f_fail=$s198f_fail"
} >> "$MANIFEST"
note "198F manifest written: $MANIFEST"

# The top-level 30-cell seal is emitted ONLY after: every supported cohort passed
# in THIS run, the total is exactly 30 (12 + 18), no fuse trip, no duplicate wake,
# and the tree stayed clean at SEAL_COMMIT. reply-cap-enqueue and shared-region
# enqueue are UNSUPPORTED policy exclusions (never counted).
if [[ "$s198f_fail" -eq 0 && "$all_ok" -eq 1 && "$first_cohort_cells" -eq 12 \
      && "$ipc_send_cells" -eq 18 && "$total_cells" -eq 30 \
      && "$fuse_total" -eq 0 && "$dupwake_total" -eq 0 ]]; then
  echo "STAGE_198F_COMPLETE_RETIREMENT_SEAL_HEADER commit=$SEAL_COMMIT run_id=$RUN_ID manifest=$MANIFEST result=ok"
  echo "STAGE_198F_COMPLETE_RETIREMENT_SEAL arches=3 first_cohort_classes=4 first_cohort_cells=12 ipc_send_classes=6 ipc_send_cells=18 total_classes=10 total_live_cells=30 reply_cap_enqueue=unsupported shared_region_enqueue=unsupported fuse_trips=0 duplicate_wakes=0 result=ok"
  exit 0
fi

echo "STAGE_198F_COMPLETE_RETIREMENT_SEAL arches=3 first_cohort_cells=$first_cohort_cells ipc_send_cells=$ipc_send_cells total_live_cells=$total_cells fuse_trips=$fuse_total duplicate_wakes=$dupwake_total result=fail"
exit 1
