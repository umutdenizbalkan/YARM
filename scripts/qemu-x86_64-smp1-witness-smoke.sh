#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP1 §3/§5 — the x86_64 two-CPU IPI and TLB witness, with bounded contention.
#
#   LOGDIR=...        build + boot logs (default /tmp/qemu-x86_64-smp1-witness)
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   REGRADE=1         grade $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS=...  boot ceiling (default 300)
#   SMP1_MUTUAL_ROUNDS=...   mutual rounds the build runs (default 4; the QEMU-LOCK3 build runs 12)
#   SMP1_EXTRA_WAKES_01/10=… reschedule IPIs CPU0->CPU1 / CPU1->CPU0 published on top of the two
#                            wakes (default 0; QEMU-LOCK3 publishes one per round it does not hold)
#
# One `-smp 2` boot of the cross-CPU reply profile with the `x86-smp1-witness` build and
# `yarm.x86_64_smp1_witness=1`. Two kernel-built tasks with distinct context patterns — the
# server on CPU 1 (address space A), the client on CPU 0 (address space B) — run
# `src/arch/x86_64/smp1_witness.S`:
#
#   wakes    — client NR6 wakes the BLOCKED server (CPU 0 -> idle CPU 1; AP saved-frame resume);
#              server NR7 wakes the blocked client (CPU 1 -> CPU 0; production selection). Each
#              resumed task checks rbx rbp r12..r15 and its FXSAVE image (FCW, MXCSR, XMM0..15).
#   resident — 8 serial rounds, alternating direction: the requester replaces the target's W
#              through NR 3 while the target is RESIDENT in ring 3, checking ten GPRs, RFLAGS
#              (CF PF AF ZF SF OF DF) and its FXSAVE image around a flag-neutral dwell. CPU 0's
#              timer keeps ticking throughout.
#   mutual   — 4 rounds in which both tasks cross a barrier and replace each other's W at once,
#              so each CPU waits (interrupts masked, no lock held) for an ACK from a CPU that is
#              itself in the kernel waiting.
#
# Graded from the owners, per round and in order: the requester's arm and the residency-probe
# re-point (on the requester's CPU only, to a frame never mapped there before); the VM owner's
# pinned displacement; shootdown completion with the target's mailbox — ACK generation equal to
# the request generation, and the privilege level the ACK was produced in; the settle of exactly
# the displaced frame, only after that; then the target's verdict — W replaced, R still the value
# it primed (no CR3 reload in between, so only the targeted invalidation explains the new W),
# context intact. Every graded line is emitted synchronously; the ring-buffered log can drop
# lines while both CPUs log.
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
  FATAL_RE="KERNEL PANIC|RUST PANIC|panicked at|DOUBLE FAULT|Unhandled|BOOTSTRAP_ERROR|X86_TLB_SHOOTDOWN_FAIL|X86_USER_FPU_HOME_UNAUTHENTICATED|X86_AP_SAVED_RESUME_REFUSED|SMP1_[A-Z0-9_]*(FAIL|STALE_AFTER_ACK|RESIDENCY_LOST|UNEXPECTED|RETURNED|EXHAUSTED)"
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
[[ "$(count "X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1")" == $((1 + ${SMP1_EXTRA_WAKES_01:-0})) ]] || die "CPU0->CPU1 wake requests != $((1 + ${SMP1_EXTRA_WAKES_01:-0}))"
before "$S_BLOCK1" "$WAKE01" "the server is blocked (CPU 1 idle) before the CPU0->CPU1 wake"
AP_RESUME=$(linere "X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved .* tid=$S_TID ")
before "$WAKE01" "$AP_RESUME" "the AP saved-frame resume follows the wake"
S_CTX=$(line_of "SMP1_USER cpu=1 SMP1_SERVER_RESUME_CONTEXT_OK cpu=1 path=ap_saved_frame gprs=6 fxsave=1 result=ok")
before "$AP_RESUME" "$S_CTX" "the server's context check follows its AP saved-frame resume"
C_BLOCK1=$(line_of "X86_SMP_ORACLE_BLOCKED cpu=0 tid=$C_TID endpoint=7 wait_gen=1")
WAKE10=$(line_of "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")
[[ "$(count "X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0")" == $((1 + ${SMP1_EXTRA_WAKES_10:-0})) ]] || die "CPU1->CPU0 wake requests != $((1 + ${SMP1_EXTRA_WAKES_10:-0}))"
before "$C_BLOCK1" "$WAKE10" "the client is blocked before the CPU1->CPU0 wake"
C_CTX=$(line_of "SMP1_USER cpu=0 SMP1_CLIENT_RESUME_CONTEXT_OK cpu=0 path=production_selection gprs=6 fxsave=1 reply=ok result=ok")
before "$WAKE10" "$C_CTX" "the client's context check follows the wake"
[[ "$(count "X86_BSP_SAVED_DISPATCH_OK")" == 0 ]] || die "the retired oracle BSP resume ran"

# ── TLB rounds. Serial round k: odd k -> the client (CPU 0) replaces the server's W while the
# server is resident on CPU 1; even k -> the mirror. Then MUTUAL rounds: both replace each
# other's W at once, so each CPU waits for an ACK while the other is in the kernel doing the same.
SERIAL=8; MUTUAL=${SMP1_MUTUAL_ROUNDS:-4}; PER_TARGET=$((SERIAL / 2))
lines() { rg -a -n -- "$1" "$NORM" | cut -d: -f1; }
nth() { sed -n "${2}p" <<<"$1"; }
for who in server client; do
  if [[ $who == server ]]; then tg=1; rq=0; as=$S_AS; else tg=0; rq=1; as=$C_AS; fi
  ARM=$(lines "^SMP1_USER cpu=$rq SMP1_ARM_R target=$who$")
  REP=$(lines "^SMP1_WITNESS_R_REPOINTED target=$who asid=$as va=0x20090000 frame=[0-9]+ new_phys=0x[0-9a-f]+ invalidated_on_cpu=$rq ok=1$")
  DISP=$(lines "^SMP1_VM_DISPLACED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+ pinned=1 requester_cpu=$rq$")
  COMP=$(lines "^SMP1_VM_SHOOTDOWN_COMPLETE asid=$as va=0x20080000 acked=1 requester_cpu=$rq target_cpu=$tg ")
  SETL=$(lines "^SMP1_VM_DISPLACED_SETTLED asid=$as va=0x20080000 ")
  VERD=$(lines "^SMP1_USER cpu=$tg SMP1_TARGET_OBSERVED cpu=$tg w=replaced r=stale context=ok result=ok$")
  MUTV=$(lines "^SMP1_USER cpu=$tg SMP1_MUTUAL_OBSERVED cpu=$tg w=replaced result=ok$")
  n() { [[ -z "$1" ]] && echo 0 || wc -l <<<"$1"; }
  [[ "$(n "$ARM")" == $PER_TARGET && "$(n "$REP")" == $PER_TARGET ]] || die "$who: arms/re-points != $PER_TARGET"
  [[ "$(n "$VERD")" == $PER_TARGET ]] || die "$who: resident verdicts != $PER_TARGET"
  [[ "$(n "$MUTV")" == $MUTUAL ]] || die "$who: mutual verdicts != $MUTUAL"
  total=$((PER_TARGET + MUTUAL))
  [[ "$(n "$DISP")" == $total ]] || die "$who: pinned displacements in its address space != $total"
  [[ "$(countre "^SMP1_VM_SHOOTDOWN_COMPLETE asid=$as va=0x20080000 ")" == $total ]] || die "$who: shootdowns != $total"
  [[ "$(n "$COMP")" == $total ]] || die "$who: acknowledged shootdowns != $total"
  [[ "$(n "$SETL")" == $total ]] || die "$who: settles != $total"
  for ((k = 1; k <= total; k++)); do
    before "$(nth "$DISP" $k)" "$(nth "$COMP" $k)" "$who #$k: displaced (pinned) before the shootdown completes"
    before "$(nth "$COMP" $k)" "$(nth "$SETL" $k)" "$who #$k: ACK before the displaced backing is settled"
    (( k < total )) && before "$(nth "$SETL" $k)" "$(nth "$DISP" $((k + 1)))" "$who #$k: settled before the next displacement"
  done
  for ((k = 1; k <= PER_TARGET; k++)); do
    before "$(nth "$ARM" $k)" "$(nth "$REP" $k)" "$who round $k: arm -> R re-pointed on the requester only"
    before "$(nth "$REP" $k)" "$(nth "$DISP" $k)" "$who round $k: R re-pointed before the displacement"
    before "$(nth "$COMP" $k)" "$(nth "$VERD" $k)" "$who round $k: the resident target's verdict follows the ACK"
    (( k < PER_TARGET )) && before "$(nth "$VERD" $k)" "$(nth "$ARM" $((k + 1)))" "$who round $k: verdict before the next arm"
  done
  # Every shootdown: the ACK generation IS the request generation, and a resident target's ACK
  # came from ring 3 (serial rounds). Mutual rounds may be answered in ring 0 by the target's own
  # wait (X86_TLB_OWN_REQUEST_SERVICED_IN_WAIT).
  k=0
  while IFS= read -r L; do
    k=$((k + 1))
    [[ "$(field "$L" target_req_gen)" == "$(field "$L" target_ack_gen)" ]] || die "$who #$k: ACK generation != request generation ($L)"
    if (( k <= PER_TARGET )); then
      [[ "$L" == *"target_origin=user" ]] || die "$who round $k: the ACKing 0xF1 did not interrupt ring 3 ($L)"
    else
      [[ "$L" == *"target_origin=user" || "$L" == *"target_origin=kernel" ]] || die "$who mutual $((k - PER_TARGET)): no origin ($L)"
    fi
  done < <(rg -a -o "SMP1_VM_SHOOTDOWN_COMPLETE asid=$as va=0x20080000 acked=1 .*" "$NORM")
  # Each settle names the frame its own displacement pinned.
  paste -d' ' <(rg -a -o "SMP1_VM_DISPLACED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+" "$NORM" | sed 's/.*old_phys=//') \
              <(rg -a -o "SMP1_VM_DISPLACED_SETTLED asid=$as va=0x20080000 old_phys=0x[0-9a-f]+" "$NORM" | sed 's/.*old_phys=//') \
    | while read -r d t; do [[ "$d" == "$t" ]] || echo "mismatch $d $t"; done | rg -q mismatch && die "$who: a settle names a different frame than its displacement"
  note "$who: $PER_TARGET resident rounds + $MUTUAL mutual rounds, every shootdown ACKed at its own generation"
done
SERVICED=$(countre "^X86_TLB_OWN_REQUEST_SERVICED_IN_WAIT cpu=[01] ")
(( SERVICED >= 1 )) || die "no mutual round overlapped (no request was answered inside a wait): contention not exercised"
note "requests answered inside the target's own ACK wait: $SERVICED"

# ── State-derived per-CPU summary. ─────────────────────────────────────────────────────────
for cpu in 0 1; do
  L="$(rg -a -o "SMP1_WITNESS_CPU cpu=$cpu .*" "$NORM" | head -1)"
  [[ -n "$L" ]] || { die "no summary for cpu $cpu"; continue; }
  w=$(field "$L" wake_arrivals); k=$(field "$L" kernel_origin); u=$(field "$L" user_origin)
  [[ -n "$w" && "$w" == "$((k + u))" ]] || die "cpu$cpu arrivals do not split by origin ($L)"
  (( u >= PER_TARGET )) || die "cpu$cpu: fewer than $PER_TARGET arrivals interrupted ring 3 ($L)"
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
echo "SMP1_WITNESS_SEAL arch=x86_64 smp=2 wakes=2 idle_target=1 ap_saved_resume=1 production_selection=1 resident_rounds=$SERIAL mutual_rounds=$MUTUAL shootdowns=$((2 * (PER_TARGET + MUTUAL))) serviced_in_wait=$SERVICED residency_proven=$SERIAL settled_after_ack=$((2 * (PER_TARGET + MUTUAL))) contexts=2+$SERIAL result=ok"
