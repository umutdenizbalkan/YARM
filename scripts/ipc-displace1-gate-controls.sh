#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# IPC-DISPLACE1-MARKER1 smoke-gate fixture controls.
#
# Usage: scripts/ipc-displace1-gate-controls.sh <arch> <smp> <fixture boot.log> <outdir> [variant...]
#
# <fixture> is the raw boot.log of a passing scripts/qemu-ipc-displace1-witness-smoke.sh run.
#
# Replays a recorded serial log through a PATH shim named qemu-system-<arch>, so the REAL,
# unmodified core smoke of that architecture grades it, then grades the same log with the
# witness grader. Each variant is a deliberate mutation of the fixture. The artifacts the smoke
# binds must already exist in this tree (built by the witness run the fixture came from).
# Expected: legit passes; every other variant fails (core smoke and/or witness grader).
set -uo pipefail
WT=$(cd "$(dirname "$0")/.." && pwd)
ARCH=$1 SMP=$2 FIX=$(realpath "$3") OUT=$(realpath -m "$4"); shift 4
VARIANTS=("${@:-legit failed_closed publish_race no_unwind dup_delivery missing_delivery renamed_failed_closed renamed_settled}")
read -r -a VARIANTS <<<"${VARIANTS[*]}"
mkdir -p "$OUT"
case "$ARCH" in
  x86_64)  Q=qemu-system-x86_64; SMOKE=scripts/qemu-x86_64-core-smoke.sh; FEATURE=x86-shared-region-direct-oracle
           DEST=build-x86_64/kernel_boot.elf; INITRD=build-x86_64/initramfs-core.cpio ;;
  aarch64) Q=qemu-system-aarch64; SMOKE=scripts/qemu-aarch64-core-smoke.sh; FEATURE=aarch64-shared-region-direct-oracle
           DEST=build-aarch64/yarm-aarch64.bin; INITRD=build-aarch64/initramfs-core.cpio ;;
  riscv64) Q=qemu-system-riscv64; SMOKE=scripts/qemu-riscv64-core-smoke.sh; FEATURE=riscv-shared-region-direct-oracle
           DEST=build-riscv64/yarm-riscv64.bin; INITRD=build-riscv64/initramfs-core.cpio ;;
esac
R1=$(grep -a -m1 -oE 'r1_tid=[0-9]+' "$FIX" | cut -d= -f2)
R2=$(grep -a -m1 -oE 'r2_tid=[0-9]+' "$FIX" | cut -d= -f2)
SHIM="$OUT/shim"; mkdir -p "$SHIM"
SUMMARY="$OUT/summary.txt"; : >"$SUMMARY"

mutate() {  # $1 variant, $2 out
  local v=$1 o=$2
  case "$v" in
    legit) cp "$FIX" "$o" ;;
    # A genuine fail-closed receive: an unwind that did NOT restore the entering incarnation.
    failed_closed) awk -v r2="$R2" '{print} /IPC_RECV_BLOCK_SPLIT_SETTLED .*reason=receiver_already_waiting/ && !d {print "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu=0 tid=" r2 " phase=publish outcome=incarnation_moved reason=receiver_already_waiting"; d=1}' "$FIX" >"$o" ;;
    # A true queue-publication race, which no witness round creates.
    publish_race) awk -v r2="$R2" '{print} /D2_RECV_WAITER_REFUSED / && !d {print "D2_PUBLISH_RACE_UNWIND endpoint=split tid=" r2; print "D2_RECV_WAITER_RACE_UNWIND tid=" r2; d=1}' "$FIX" >"$o" ;;
    # One exact-unwind completion missing.
    no_unwind) awk -v r2="$R2" '$0 ~ ("D2_RECV_SPLIT_UNWIND_OK .*tid=" r2 " ") && !d {d=1; next} {print}' "$FIX" >"$o" ;;
    # One delivery to R1 duplicated (kernel line and R1's own report).
    dup_delivery) awk -v r1="$R1" '{print} $0 ~ ("IPC_RECV_DELIVER_TO_WAITER tid=" r1 " ") && !d {print; d=1} $0 ~ ("tid=" r1 " msg=IPCD1_WITNESS_R1 round=3 ") && !e {print; e=1}' "$FIX" >"$o" ;;
    # One delivery to R1 missing.
    missing_delivery) awk -v r1="$R1" '$0 ~ ("IPC_RECV_DELIVER_TO_WAITER tid=" r1 " ") && ++n == 5 {next} {print}' "$FIX" >"$o" ;;
    # Control 6: a genuinely unrestored refusal whose FAILED_CLOSED diagnostic was renamed away —
    # its exact-unwind completion is gone and its settle line tells the truth about the outcome.
    renamed_failed_closed) awk -v r2="$R2" '$0 ~ ("D2_RECV_SPLIT_UNWIND_OK .*tid=" r2 " ") && ++n == 7 {next}
        $0 ~ ("IPC_RECV_BLOCK_SPLIT_SETTLED .*tid=" r2 " ") && ++m == 7 {sub(/outcome=restored resumable=1/, "outcome=incarnation_moved resumable=0")} {print}' "$FIX" >"$o" ;;
    # Control 6: the settled-refusal marker renamed (no positive settlement evidence left).
    renamed_settled) sed 's/IPC_RECV_BLOCK_SPLIT_SETTLED/IPC_RECV_BLOCK_SPLIT_REFUSAL_NOTE/' "$FIX" >"$o" ;;
    *) echo "unknown variant $v"; return 1 ;;
  esac
}

for v in "${VARIANTS[@]}"; do
  d="$OUT/$v"; mkdir -p "$d"
  mutate "$v" "$d/fixture.log" || continue
  printf '#!/usr/bin/env bash\ncat %q\nexit 0\n' "$d/fixture.log" >"$SHIM/$Q"; chmod +x "$SHIM/$Q"
  ( cd "$WT" && env PATH="$SHIM:$PATH" \
      KERNEL_IMAGE="$DEST" ARTIFACT_REQUIRE="kernel=$FEATURE initramfs=ipc-displace1-witness" \
      INITRAMFS_IMAGE="$INITRD" KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.ipc_displace1_witness=1" \
      QEMU_SMP="$SMP" LOGFILE="$d/boot.log" SMOKE_LOG="$d/smoke.log" TIMEOUT_SECS=30 \
      YARM_MODE_ISOLATION=0 TERMINAL_IDLE_MARKER="IPCD1_WITNESS_DONE" \
      "$SMOKE" >"$d/core-smoke.log" 2>&1 )
  rc=$?
  tr '\r' '\n' <"$d/boot.log" >"$d/boot.norm.log"
  cmp -s "$d/fixture.log" "$d/boot.log" && same=1 || same=0
  python3 -I "$WT/scripts/grade-ipc-displace1-witness.py" --arch "$ARCH" --smp "$SMP" --rounds 16 \
    --core-rc "$rc" --core-log "$d/smoke.log" --core-log "$d/core-smoke.log" "$d/boot.norm.log" >"$d/grade.txt" 2>&1
  seal=$(grep -m1 'IPC_DISPLACE1_WITNESS_SEAL' "$d/grade.txt")
  core=$(grep -oE 'core_smoke=[^ ]+' <<<"$seal"); wit=$(grep -oE 'witness=[a-z]+' <<<"$seal"); res=$(grep -oE 'result=[a-z]+$' <<<"$seal")
  reasons=$(grep -E '^\[ipc-displace1-grade\]\[core\]' "$d/grade.txt" | sed 's/^\[ipc-displace1-grade\]\[core\] //' | cut -c1-110 | paste -sd'|' -)
  echo "GATEFIX arch=$ARCH variant=$v replayed_exact=$same core_rc=$rc $core $wit $res core_reasons=[${reasons}]" | tee -a "$SUMMARY"
done
