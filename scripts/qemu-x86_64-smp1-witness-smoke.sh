#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP1 §3 — the x86_64 two-CPU IPI and TLB witness.
#
#   LOGDIR=...        build + boot logs (default /tmp/qemu-x86_64-smp1-witness)
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   REGRADE=1         grade $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS=...  boot ceiling (default 300)
#
# One `-smp 2` boot of the cross-CPU reply profile with the `x86-smp1-witness` build and
# `yarm.x86_64_smp1_witness=1`. Two kernel-built tasks with distinct context patterns — the
# server on CPU 1 (address space A), the client on CPU 0 (address space B) — run
# `src/arch/x86_64/smp1_witness.S`:
#
#   wakes  — client NR6 wakes the BLOCKED server (CPU 0 -> idle CPU 1; AP saved-frame resume);
#            server NR7 wakes the blocked client (CPU 1 -> CPU 0; production selection). Each
#            resumed task checks rbx rbp r12..r15 and its FXSAVE pattern (FCW, MXCSR, XMM0..15).
#   T01    — the client replaces W in A through NR 3 while the server is RESIDENT in ring 3 on
#            CPU 1, checking its full pattern (ten GPRs, RFLAGS arithmetic bits, FXSAVE image)
#            around a flag-neutral dwell the IPI lands in.
#   T10    — the mirror: the server replaces W in B while the client is resident on CPU 0 (whose
#            timer keeps ticking), with DF set in the client's pattern.
#
# Graded from the owners, in order: the requester's arm and the residency-probe re-point; the VM
# owner's pinned displacement; the TLB post and its exact generation; the target's ACK from ring 3
# (the mailbox the target's own stub wrote); shootdown completion before the displaced backing is
# settled; then the target's verdict — W replaced, R still the OLD translation (no CR3 reload in
# between, so only the targeted invalidation can explain the new W), context intact. Every graded
# line is emitted synchronously; the ring-buffered log can drop lines while both CPUs log.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-smp1-witness}
TIMEOUT_SECS=${TIMEOUT_SECS:-300}
KTARGET=targets/x86_64-yarm-none.json
KPROFILE=x86-none
BUILD_STD=core,alloc,compiler_builtins,panic_abort
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
fail=0
note() { echo "[smp1-witness] $*"; }
die()  { echo "[smp1-witness][fail] $*"; fail=1; }
seal_fail() { echo "SMP1_WITNESS_SEAL arch=x86_64 smp=2 result=fail reason=$1"; exit 1; }

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  note "building base artifacts into $BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 || seal_fail build
  note "building kernel_boot with --features x86-smp1-witness"
  cargo build -Z "build-std=$BUILD_STD" -Z json-target-spec --target "$KTARGET" \
    --profile "$KPROFILE" --no-default-features --features x86-smp1-witness \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 || seal_fail kernel_build
  cp "target/x86_64-yarm-none/$KPROFILE/kernel_boot" "$BUILD_DIR/kernel_boot.elf" || seal_fail copy
fi
[[ -s "$BUILD_DIR/kernel_boot.elf" && -s "$BUILD_DIR/initramfs-core.cpio" ]] || seal_fail no_artifacts

if [[ "${REGRADE:-0}" != "1" ]]; then
  source scripts/lib/qemu-x86-deterministic.sh
  QEMU_ARGV=(
    qemu-system-x86_64 -machine q35 -cpu qemu64 -m 512M -smp 2
    -nographic -monitor none -serial stdio -no-reboot -no-shutdown
    -kernel "$BUILD_DIR/kernel_boot.elf" -initrd "$BUILD_DIR/initramfs-core.cpio"
    -append "console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.x86_64_ipccall_direct_smp_reply=1 yarm.ap_user_dispatch=1 yarm.x86_64_smp1_witness=1"
  )
  QEMU_TERMINAL_MARKERS=(
    "SMP1_WITNESS_SUMMARY result=ok"
    "X86_SMP_ORACLE_BLOCKED cpu=1 tid=20205 endpoint=6 wait_gen=2"
    "X86_SMP_ORACLE_BLOCKED cpu=0 tid=21205 endpoint=7 wait_gen=2"
  )
  FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|Unhandled|BOOTSTRAP_ERROR|X86_TLB_SHOOTDOWN_FAIL|X86_USER_FPU_HOME_UNAUTHENTICATED|X86_AP_SAVED_RESUME_REFUSED|SMP1_[A-Z0-9_]*(FAIL|STALE_AFTER_ACK|RESIDENCY_LOST|UNEXPECTED|RETURNED)"
  echo "#HOST_QEMU ${QEMU_ARGV[*]} $(qemu-system-x86_64 --version | head -1)" >"$BOOT_LOG.host"
  note "booting -smp 2 (ceiling ${TIMEOUT_SECS}s)"
  qemu_run_deterministic "$BOOT_LOG" "$FATAL_RE" "$TIMEOUT_SECS" "${QEMU_ARGV[@]}"
  rc=$?
  note "qemu lifecycle: ${QEMU_LIFECYCLE_RESULT} (rc=$rc)"
  (( rc == 0 )) || { tr '\r' '\n' <"$BOOT_LOG" >"$NORM"; seal_fail "$QEMU_LIFECYCLE_RESULT"; }
fi
[[ -s "$BOOT_LOG" ]] || seal_fail no_boot_log
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { rg -a -c -F -- "$1" "$NORM" 2>/dev/null || echo 0; }
countre() { rg -a -c -- "$1" "$NORM" 2>/dev/null || echo 0; }
line_of() { rg -a -n -F -- "$1" "$NORM" | head -1 | cut -d: -f1; }
linere() { rg -a -n -- "$1" "$NORM" | head -1 | cut -d: -f1; }
field() { sed -n "s/.*[ ]$2=\([0-9a-fx]*\).*/\1/p" <<<"$1"; }
before() { # a b what — line a precedes line b, both present
  [[ -n "$1" && -n "$2" ]] && (( $1 < $2 )) || die "order: $3 (lines ${1:-missing} < ${2:-missing})"
}

PROV="$(rg -a -o 'SMP1_WITNESS_PROVISIONED .*' "$NORM" | head -1)"
S_TID=$(field "$PROV" server_tid); S_AS=$(field "$PROV" server_asid)
C_TID=$(field "$PROV" client_tid); C_AS=$(field "$PROV" client_asid)
[[ -n "$S_TID" && -n "$C_TID" && -n "$S_AS" && -n "$C_AS" && "$S_AS" != "$C_AS" ]] \
  || die "provisioning identities missing or not distinct ($PROV)"
[[ "$S_TID" == 20205 && "$C_TID" == 21205 ]] || die "identities differ from the terminal markers"

# ── Wakes in both directions, the idle target, both resume paths, both context checks. ──────
S_BLOCK1=$(line_of "X86_SMP_ORACLE_BLOCKED cpu=1 tid=$S_TID endpoint=6 wait_gen=1")
WAKE01=$(line_of "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")
[[ "$(count "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")" == 1 ]] || die "CPU0->CPU1 wake requests != 1"
before "$S_BLOCK1" "$WAKE01" "the server is blocked (CPU 1 idle) before the CPU0->CPU1 wake"
AP_RESUME=$(linere "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved .* tid=$S_TID ")
before "$WAKE01" "$AP_RESUME" "the AP saved-frame resume follows the wake"
S_CTX=$(line_of "SMP1_USER cpu=1 SMP1_SERVER_RESUME_CONTEXT_OK cpu=1 path=ap_saved_frame gprs=6 fxsave=1 result=ok")
before "$AP_RESUME" "$S_CTX" "the server's context check follows its AP saved-frame resume"
C_BLOCK1=$(line_of "X86_SMP_ORACLE_BLOCKED cpu=0 tid=$C_TID endpoint=7 wait_gen=1")
WAKE10=$(line_of "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")
[[ "$(count "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")" == 1 ]] || die "CPU1->CPU0 wake requests != 1"
before "$C_BLOCK1" "$WAKE10" "the client is blocked before the CPU1->CPU0 wake"
C_CTX=$(line_of "SMP1_USER cpu=0 SMP1_CLIENT_RESUME_CONTEXT_OK cpu=0 path=production_selection gprs=6 fxsave=1 reply=ok result=ok")
before "$WAKE10" "$C_CTX" "the client's context check follows the wake"
[[ "$(count "X86_BSP_SAVED_DISPATCH_OK")" == 0 ]] || die "the retired oracle BSP resume ran"

# ── One TLB cell: requester R replaces W in the target's address space while it is resident. ──
tlb_cell() { # name requester_cpu target_cpu target_asid target_word
  local name=$1 rq=$2 tg=$3 as=$4 who=$5
  local arm rep disp comp settled done verdict line gen
  arm=$(line_of "SMP1_USER cpu=$rq SMP1_ARM_R target=$who")
  rep=$(linere "SMP1_WITNESS_R_REPOINTED target=$who asid=$as va=0x20090000 new_phys=0x[0-9a-f]+ invalidated_on_cpu=$rq ok=1")
  disp=$(linere "SMP1_VM_DISPLACED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+ pinned=1 requester_cpu=$rq$")
  comp=$(linere "SMP1_VM_SHOOTDOWN_COMPLETE asid=$as va=0x20080000 acked=1 requester_cpu=$rq target_cpu=$tg ")
  settled=$(linere "SMP1_VM_DISPLACED_SETTLED asid=$as va=0x20080000 ")
  done=$(linere "SMP1_USER cpu=$rq SMP1_$name""_REQUESTER_DONE cpu=$rq vm_map=ok result=ok")
  verdict=$(line_of "SMP1_USER cpu=$tg SMP1_$name""_TARGET_OBSERVED cpu=$tg w=replaced r=stale context=ok result=ok")
  before "$arm" "$rep" "$name: arm -> R re-pointed on the requester only"
  before "$rep" "$disp" "$name: R re-pointed before the displacement"
  before "$disp" "$comp" "$name: displaced (pinned) before the shootdown completes"
  before "$comp" "$settled" "$name: shootdown ACKed before the displaced backing is settled"
  before "$comp" "$verdict" "$name: the target's verdict follows the ACK"
  before "$comp" "$done" "$name: the requester's NR 3 returns after the ACK"
  [[ "$(countre "SMP1_VM_DISPLACED asid=$as va=0x20080000 ")" == 1 ]] || die "$name: displacements != 1"
  [[ "$(countre "SMP1_VM_DISPLACED_SETTLED asid=$as va=0x20080000 ")" == 1 ]] || die "$name: settles != 1"
  local d s
  d=$(rg -a -o "SMP1_VM_DISPLACED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+" "$NORM" | head -1 | sed 's/.*old_phys=//')
  s=$(rg -a -o "SMP1_VM_DISPLACED_SETTLED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+" "$NORM" | head -1 | sed 's/.*old_phys=//')
  [[ -n "$d" && "$d" == "$s" ]] || die "$name: the settled backing ($s) is not the displaced one ($d)"
  line=$(rg -a -o "SMP1_VM_SHOOTDOWN_COMPLETE asid=$as va=0x20080000 acked=1 .*" "$NORM" | head -1)
  gen=$(field "$line" target_req_gen)
  [[ -n "$gen" && "$gen" == "$(field "$line" target_ack_gen)" ]] || die "$name: ACK generation != request generation ($line)"
  [[ "$line" == *"target_origin=user"* ]] || die "$name: the ACKing 0xF1 did not interrupt ring 3 ($line)"
  # The ring-buffered TLB lines, when present, must name the same target and generation.
  if [[ "$(countre "X86_TLB_SHOOTDOWN_SEND target_cpu=$tg gen=$gen va=0x20080000$")" != 0 ]]; then
    [[ "$(countre "X86_TLB_SHOOTDOWN_ACK cpu=$tg gen=$gen origin=user$")" != 0 ]] || die "$name: SEND gen=$gen without its user-origin ACK"
  fi
  note "$name: requester=cpu$rq target=cpu$tg asid=$as gen=$gen displaced=$d"
}
tlb_cell T01 0 1 "$S_AS" server
tlb_cell T10 1 0 "$C_AS" client

# ── State-derived per-CPU summary. ─────────────────────────────────────────────────────────
for cpu in 0 1; do
  L="$(rg -a -o "SMP1_WITNESS_CPU cpu=$cpu .*" "$NORM" | head -1)"
  [[ -n "$L" ]] || { die "no summary for cpu $cpu"; continue; }
  w=$(field "$L" wake_arrivals); k=$(field "$L" kernel_origin); u=$(field "$L" user_origin)
  [[ -n "$w" && "$w" == "$((k + u))" ]] || die "cpu$cpu arrivals do not split by origin ($L)"
  (( u >= 1 )) || die "cpu$cpu: no 0xF1 arrival interrupted ring 3 ($L)"
  [[ "$(field "$L" tlb_req_gen)" == "$(field "$L" tlb_ack_gen)" ]] || die "cpu$cpu: a TLB request is outstanding ($L)"
  note "summary: $L"
done

# ── Both tasks block again; nothing failed, stalled or leaked. ────────────────────────────────
[[ "$(count "X86_SMP_ORACLE_BLOCKED cpu=1 tid=$S_TID endpoint=6 wait_gen=2")" == 1 ]] || die "server did not block again on cpu 1"
[[ "$(count "X86_SMP_ORACLE_BLOCKED cpu=0 tid=$C_TID endpoint=7 wait_gen=2")" == 1 ]] || die "client did not block again on cpu 0"
for bad in "X86_TLB_SHOOTDOWN_FAIL" "X86_USER_FPU_COMMIT_REFUSED" "X86_USER_FPU_HOME_UNAUTHENTICATED" \
           "X86_AP_SAVED_RESUME_REFUSED" "KERNEL PANIC" "panicked at" "Unhandled" "DOUBLE FAULT"; do
  [[ "$(count "$bad")" == 0 ]] || die "fatal/refusal marker: $bad"
done
[[ "$(countre "SMP1_USER .*(FAIL|STALE_AFTER_ACK|RESIDENCY_LOST|UNEXPECTED|RETURNED)")" == 0 ]] || die "a witness program reported failure"

if (( fail )); then seal_fail grading; fi
echo "SMP1_WITNESS_SEAL arch=x86_64 smp=2 wakes=2 idle_target=1 ap_saved_resume=1 production_selection=1 tlb_cells=2 user_origin_acks=2 residency_proven=2 old_backing_settled_after_ack=2 contexts=4 result=ok"
