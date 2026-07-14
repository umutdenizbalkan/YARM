#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 198B1 Part A — ARTIFACT BUILD INTEGRITY SEAL.
#
# Proves the artifact build pipeline fails CLOSED and never publishes or reuses a
# stale kernel. Root cause it guards against (Stage 198B): a Rust compile error
# under `set +e` was swallowed, a stale kernel remained at the output path, QEMU
# booted it, and old 128-byte DebugLog behavior looked current.
#
# Two proofs:
#   POSITIVE — each of the three per-arch build scripts, on a clean build, emits
#     `ARTIFACT_BUILD_INTEGRITY arch=<arch> ... result=ok` (freshness + required
#     markers present + forbidden/obsolete markers absent + manifest recorded).
#   NEGATIVE — a simulated compiler failure (a `compile_error!` injected into a
#     kernel source) makes the build exit NONZERO, leaves NO artifact at the
#     expected output path, and the ordinary-cap seal then REFUSES to run.
#
# Emits: ARTIFACT_BUILD_INTEGRITY_SEAL arches=3 stale_artifact_acceptance=0
#        failed_build_rejected=1 result=ok
set -euo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/artifact-build-integrity}
mkdir -p "$LOGDIR"
fail=0
note() { echo "[integrity-seal] $*"; }
die()  { echo "[integrity-seal][fail] $*"; fail=1; }

declare -A BUILD=(
  [x86_64]="scripts/build-qemu-x86_64-artifacts.sh"
  [aarch64]="scripts/build-qemu-aarch64-artifacts.sh"
  [riscv64]="scripts/build-qemu-riscv64-artifacts.sh"
)
declare -A KBIN=(
  [x86_64]="build-x86_64/kernel_boot.elf"
  [aarch64]="build-aarch64/yarm-aarch64.bin"
  [riscv64]="build-riscv64/yarm-riscv64.bin"
)

# ── 1. POSITIVE: fresh build per arch must emit its integrity line ──
integrity_ok=0
for arch in x86_64 aarch64 riscv64; do
  note "clean build: $arch"
  if ! ARTIFACTS_STRICT=1 bash "${BUILD[$arch]}" > "$LOGDIR/${arch}-build.log" 2>&1; then
    die "$arch clean build exited nonzero (see $LOGDIR/${arch}-build.log)"; continue
  fi
  if grep -qa "ARTIFACT_BUILD_INTEGRITY arch=${arch} stale_artifact_acceptance=0 failed_build_rejected=1 result=ok" "$LOGDIR/${arch}-build.log"; then
    note "$arch integrity line present + marker contract satisfied"
    integrity_ok=$((integrity_ok + 1))
  else
    die "$arch integrity line ABSENT (build did not fail closed / verify markers)"
  fi
done

# ── 2. NEGATIVE: a simulated compile failure must fail closed + leave no stale artifact ──
INJECT_FILE="src/lib.rs"
BACKUP="$LOGDIR/inject.bak"
cp "$INJECT_FILE" "$BACKUP"
restore_inject() { cp "$BACKUP" "$INJECT_FILE"; }
trap restore_inject EXIT
printf '\ncompile_error!("STAGE_198B1_ARTIFACT_INTEGRITY_NEGATIVE_TEST injected failure");\n' >> "$INJECT_FILE"
note "injected compile_error into $INJECT_FILE; expecting x86_64 build to FAIL CLOSED"

neg_rc=0
ARTIFACTS_STRICT=1 bash scripts/build-qemu-x86_64-artifacts.sh > "$LOGDIR/negative-build.log" 2>&1 || neg_rc=$?
restore_inject
trap - EXIT
note "restored $INJECT_FILE"

failed_build_rejected=0
if [[ "$neg_rc" -ne 0 ]]; then
  note "negative build correctly exited nonzero (rc=$neg_rc)"
  failed_build_rejected=1
else
  die "negative build exited 0 despite an injected compile_error"
fi

stale_artifact_acceptance=1
if [[ -f "${KBIN[x86_64]}" ]]; then
  die "STALE kernel artifact remained at ${KBIN[x86_64]} after a failed build"
else
  note "no stale kernel artifact left at ${KBIN[x86_64]} (fail-closed confirmed)"
  # The seal must REFUSE to run while the x86_64 kernel artifact is absent.
  seal_rc=0
  QEMU_SMOKE_STRICT=1 bash scripts/qemu-second-cohort-ordinary-cap-seal.sh > "$LOGDIR/seal-refuses-absent.log" 2>&1 || seal_rc=$?
  if [[ "$seal_rc" -ne 0 ]] && grep -qa "reason=missing_artifacts" "$LOGDIR/seal-refuses-absent.log"; then
    note "ordinary-cap seal correctly REFUSED the absent artifact (rc=$seal_rc)"
    stale_artifact_acceptance=0
  else
    die "ordinary-cap seal did NOT refuse the absent artifact (rc=$seal_rc)"
  fi
fi

# ── 3. Restore a valid x86_64 artifact so downstream seals can run ──
note "rebuilding x86_64 to restore a valid artifact"
ARTIFACTS_STRICT=1 bash scripts/build-qemu-x86_64-artifacts.sh > "$LOGDIR/x86_64-restore-build.log" 2>&1 || die "x86_64 restore build failed"
[[ -f "${KBIN[x86_64]}" ]] || die "x86_64 artifact still absent after restore build"

# ── 4. Verdict ──
if [[ "$integrity_ok" -eq 3 && "$failed_build_rejected" -eq 1 && "$stale_artifact_acceptance" -eq 0 && "$fail" -eq 0 ]]; then
  echo "ARTIFACT_BUILD_INTEGRITY_SEAL arches=3 stale_artifact_acceptance=0 failed_build_rejected=1 result=ok"
  exit 0
fi
echo "ARTIFACT_BUILD_INTEGRITY_SEAL arches=3 stale_artifact_acceptance=${stale_artifact_acceptance} failed_build_rejected=${failed_build_rejected} integrity_ok=${integrity_ok} result=fail"
exit 1
