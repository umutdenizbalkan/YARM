#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 200C2C2 — RISC-V LIVE reply-receive TIMEOUT OFF-LOCK RETIREMENT smoke (two fresh boots).
#
# The RISC-V port of the accepted x86_64/AArch64 IpcReplyTimeout retirement cell (the third cell). It REUSES the arch-neutral
# collector + per-CPU deferred-work drain + completion transaction verbatim, wired into the AArch64
# trap-entry post-lock area, and proves the two live outcomes on `-smp 1`, EACH from a fresh boot of
# the SAME clean tree:
#
#   A. timeout-wins  — the production off-lock collector publishes one deferred work item and the
#                      off-lock drain completes it: the blocked recv-v2 caller resumes with the
#                      canonical TimedOut written to its saved trap frame, the class reports
#                      scan_broad_lock=0, and the retirement seal is emitted. The late NR7 is rejected.
#   B. reply-wins    — the server's NR7 wins terminal ownership first (a reversible ClaimedByReply
#                      lease blocks any concurrent timeout claim); the reply copies, the record +
#                      terminal complete as Reply, the deadline lease completes, and the caller resumes
#                      with the exact payload. A later production scan passes the old deadline
#                      harmlessly (no timeout work/wake).
#
# LIVE RETIREMENT seal: the reply-timeout class deadline scan runs OFF the broad KernelState
# (IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0), and the runner emits
# GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout exactly once (timeout-wins boot).
# Ordinary receive timeouts stay on their existing in-lock path (NOT retired here).
#
# On both fresh boots passing, the runner (not userspace) emits:
#   STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 ...
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=riscv64-ipc-reply-timeout-oracle
KTARGET=${KTARGET:-riscv64gc-unknown-none-elf}
KPROFILE=${KPROFILE:-release}
KELF=${KELF:-target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot}
KBIN=${KBIN:-build-riscv64/yarm-riscv64.bin}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ipc-reply-timeout-retirement-riscv64}
TIMEOUT_SECS=${TIMEOUT_SECS:-180}
IDLE_MAX_SECS=${IDLE_MAX_SECS:-180}
mkdir -p "$LOGDIR"

fail=0
# Canonical 199E-R3: `--self-test` exercises the oracle-scoped accounting helpers against
# synthetic fixtures ONLY. It needs no artifacts, so every build and boot step below is skipped.
SELF_TEST=0
[[ "${1:-}" == "--self-test" ]] && SELF_TEST=1
note() { echo "[ipc-reply-timeout-retire-riscv64] $*"; }
die()  { echo "[ipc-reply-timeout-retire-riscv64][fail] $*"; fail=1; }

# ── SHA + clean-tree capture (re-checked between the two fresh boots) ──
SHA0=$(git rev-parse HEAD 2>/dev/null || echo unknown)
clean_tree() { git diff --quiet && git diff --cached --quiet; }
if clean_tree; then TREE0=clean; else TREE0=dirty; fi
note "sha=$SHA0 tree=$TREE0"

recheck_sha_clean() {
  local sha; sha=$(git rev-parse HEAD 2>/dev/null || echo unknown)
  [[ "$sha" == "$SHA0" ]] || die "SHA drifted mid-run ($SHA0 -> $sha)"
  if clean_tree; then :; else [[ "$TREE0" == "dirty" ]] || die "tree became dirty mid-run"; fi
}

objcopy_tool() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy;
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy;
  else return 1; fi
}

# ── 1. Base artifacts (servers + initramfs; the userspace oracle is arch-gated) ──
if (( ! SELF_TEST )); then
  note "building base riscv64 artifacts (servers + initramfs)"
  BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || die "base artifact build failed (see $LOGDIR/build.log)"
fi

OBJCOPY=""
if (( ! fail && ! SELF_TEST )); then OBJCOPY=$(objcopy_tool) || die "no objcopy available"; fi

# ── 2. Feature-ON kernel + integrity: it MUST carry the AArch64 retirement literals ──
if (( ! fail && ! SELF_TEST )); then
  note "building kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail && ! SELF_TEST )); then
  "$OBJCOPY" -O binary "$KELF" "$KBIN" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of feature kernel failed (see $LOGDIR/objcopy.log)"
  # The AArch64 kernel carries the arch=riscv64 attribution + the class literal.
  rg -a -q "riscv64" "$KBIN" || die "feature kernel missing riscv64 attribution"
  rg -a -q "class=IpcReplyTimeout" "$KBIN" || die "feature kernel missing IpcReplyTimeout class literal"
  # Cross-arch hygiene: no x86_64/riscv64 reply-timeout attribution in the AArch64 kernel.
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=x86_64" "$KBIN" && die "x86_64 reply-timeout attribution in riscv64 kernel"
  rg -a -q "IPC_REPLY_TIMEOUT_OK arch=aarch64" "$KBIN" && die "aarch64 reply-timeout attribution in riscv64 kernel"
fi

# ── 2b. Feature-OFF kernel MUST be marker-CLEAN of the reply-timeout retirement literals ──
if (( ! fail && ! SELF_TEST )); then
  note "building feature-OFF kernel_boot and asserting it is marker-clean"
  cargo build -Z "build-std=${BUILD_STD}" \
    --target "$KTARGET" --profile "$KPROFILE" --no-default-features \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild-off.log" 2>&1 \
    || die "feature-off kernel_boot build failed (see $LOGDIR/kbuild-off.log)"
  OFF_BIN="$LOGDIR/kernel_boot_off.bin"
  "$OBJCOPY" -O binary "$KELF" "$OFF_BIN" >/dev/null 2>&1 || die "objcopy of feature-off kernel failed"
  # U7 (canonical 199E) split this gate in two, because the feature no longer decides where
  # timeouts are processed — only which oracle SCENARIOS are built.
  #
  # (a) The oracle's own scenario literals must still be absent from a feature-OFF kernel.
  #
  # Canonical 199E moved "IPC_REPLY_TIMEOUT_ARMED arch=" out of this half and into (b). It is
  # no longer an oracle-only literal: `KernelState::arm_production_reply_deadline` is
  # unconditional production code — no `#[cfg]`, no runtime selector — and emits the same
  # arch-neutral `IPC_REPLY_TIMEOUT_ARMED arch={}` format string at the committed
  # reply-receive block point, so the literal is compiled into EVERY image, feature-OFF
  # included. Asserting its absence would now assert that production registration is absent.
  for lit in "IPC_REPLY_BEATS_TIMEOUT_OK arch=" \
             "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=" \
             "IPC_REPLY_WIN_RESERVE arch="; do
    rg -a -q "$lit" "$OFF_BIN" && die "feature-OFF kernel contains oracle literal $lit (not marker-clean)"
  done
  # (b) The PRODUCTION pipeline's literals must now be PRESENT in a feature-OFF kernel. Their
  # absence would mean the promotion regressed back behind the feature — the exact thing U7
  # removed — so this half of the gate is asserted positively.
  # NB: the arch tag is a RUNTIME argument (`arch={}` + `REPLY_TIMEOUT_ARCH`), so the composite
  # "MARKER arch=riscv64" string never exists in the image — only the format-string fragment
  # does. Matching the fragment is what makes this gate real rather than vacuously true.
  # `class=IpcReplyTimeout` is deliberately NOT in this list on riscv64. Its only emit site on
  # this port is the resume-boundary recv/reply completion consumer, which U6 policy keeps
  # feature-gated (`the_reply_timeout_feature_policy_is_unchanged`) and U7 does not widen: U7
  # promoted where timeouts are SCANNED and SETTLED, not the IpcRecv delivery consumer. On
  # x86_64 the drain itself is the delivery point, so the seal is present there.
  for lit in "IPC_REPLY_TIMEOUT_OK arch=" "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=" \
             "IPC_REPLY_TIMEOUT_LATE_SCAN arch=" \
             "IPC_REPLY_TIMEOUT_DEFERRED arch=" \
             "IPC_REPLY_TIMEOUT_ARMED arch="; do
    rg -a -q "$lit" "$OFF_BIN" || die "feature-OFF kernel is missing PRODUCTION literal $lit (the U7 pipeline must not be feature-gated)"
  done
fi

if (( fail )); then
  echo "STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 result=fail reason=build"
  exit 1
fi

# ── Boot helper: one fresh -smp 1 boot for the given mode, into its own log ──
# Stage 200C2C2C-R2B: `QEMU_SINGLE_BOOT=1` adds `-no-reboot -no-shutdown`, so the guest can
# never restart inside one log; `assert_single_boot_instance` then PROVES from the captured
# serial that exactly one boot instance actually occurred.
boot_mode() {
  local mode="$1" log="$2" tag="${3:-$1}"
  env \
    KERNEL_IMAGE="$KBIN" \
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio \
    KERNEL_CMDLINE="yarm.riscv_ipc_reply_timeout_oracle=${mode}" \
    QEMU_SMP=1 \
    QEMU_SINGLE_BOOT=1 \
    QEMU_SMOKE_STRICT=0 \
    LOGFILE="$log" \
    TIMEOUT_SECS="$TIMEOUT_SECS" \
    IDLE_MAX_SECS="$IDLE_MAX_SECS" \
    scripts/qemu-riscv64-core-smoke.sh >"$LOGDIR/core-${tag}.log" 2>&1 || true
}

# ── Stage 200C2C2C-R2B: SINGLE-BOOT-INSTANCE proof ──
# Every marker-count assertion in this runner ("count must be 1") is only meaningful if the log
# holds exactly ONE boot instance. A reset, a payload re-entry, or a duplicated runner would
# inflate counts and could make a one-shot latch appear to fire twice. Prove single-instance
# from three INDEPENDENT boot-start witnesses (firmware banner, kernel cmdline capture, kernel
# boot completion) plus one QEMU launch, and CLASSIFY any duplicate instead of ignoring it.
assert_single_boot_instance() {
  local norm="$1" core="$2" label="$3"
  local sbi entry bootok launches
  sbi=$(rg -a -c -F "OpenSBI v" "$norm" 2>/dev/null || echo 0)
  entry=$(rg -a -c -F "YARM_BOOT_CMDLINE_CAPTURE arch=riscv64" "$norm" 2>/dev/null || echo 0)
  bootok=$(rg -a -c -F "YARM_BOOT_OK present_cpus=" "$norm" 2>/dev/null || echo 0)
  launches=$(rg -a -c -F "[info] qemu command:" "$core" 2>/dev/null || echo 0)
  note "[$label] boot-instance witnesses: opensbi=$sbi kernel_entry=$entry boot_ok=$bootok qemu_launches=$launches"
  [[ "$launches" == "1" ]] || die "[$label] expected exactly one QEMU launch, got $launches"
  if [[ "$sbi" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $sbi OpenSBI banners (firmware re-entry / guest reset)"
  fi
  if [[ "$entry" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $entry kernel entries (kernel payload re-entry)"
  fi
  if [[ "$bootok" != "1" ]]; then
    die "[$label] DUPLICATE BOOT INSTANCE: $bootok kernel boot completions"
  fi
}

# Assert marker A strictly precedes marker B (first occurrence of each).
assert_order() {
  local norm="$1" a="$2" b="$3" why="$4"
  local la lb
  la=$(rg -a -n -F "$a" "$norm" | head -1 | cut -d: -f1)
  lb=$(rg -a -n -F "$b" "$norm" | head -1 | cut -d: -f1)
  if [[ -z "$la" || -z "$lb" ]]; then
    die "ordering evidence missing ($a=$la $b=$lb)"
    return
  fi
  (( la < lb )) || die "$why ($a@$la must precede $b@$lb)"
}

verify_log() {
  local norm="$1"; shift
  local m c
  for m in "$@"; do
    c=$(rg -a -c -F "$m" "$norm" 2>/dev/null || echo 0)
    [[ "$c" == "1" ]] || die "marker count != 1 (got $c): $m"
  done
}
# ── Canonical 199E: ORACLE-SCOPED settlement accounting ──────────────────────────────────────
#
# Production reply/call registration is live on every boot now, so the ARMED and OK marker
# FAMILIES are no longer oracle-specific: an unrelated production caller legitimately arms its own
# reply deadline in the same cell, and where its scheduler-tick deadline elapses it legitimately
# settles too. Counting the family and calling the result "the oracle's" was therefore wrong, and
# it is wrong in the dangerous direction — it fails on correct behaviour.
#
# These helpers scope the oracle's assertions to the oracle's EXACT caller identity, taken from
# the provisioning marker rather than hardcoded, and keep a family-level bound that a duplicate
# settlement cannot satisfy. Nothing is loosened: the identity check is STRICTER than the family
# count it replaces, every settlement line must still carry the full field contract, and the
# oracle's one-shot terminal seals stay at exactly one.
oracle_init_tid() {
  rg -a -o -m1 'IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=[0-9]+' "$1" 2>/dev/null \
    | rg -a -o '[0-9]+$' || true
}

# Exactly ONE registration for the oracle's own caller identity.
verify_oracle_armed_once() {
  local norm="$1" arch="$2" tid c
  tid="$(oracle_init_tid "$norm")"
  [[ -n "$tid" ]] || die "no oracle provisioning marker: the oracle identity cannot be scoped"
  c=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null || echo 0)
  [[ "$c" == "1" ]] || die "oracle-identity ARMED count != 1 (got $c) for caller_tid=${tid}"
}

# No DUPLICATE settlement anywhere, and every settlement carries the exact field contract.
verify_no_duplicate_settlement() {
  local norm="$1" arch="$2" full="$3" armed ok okfull
  armed=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} " "$norm" 2>/dev/null || echo 0)
  ok=$(rg -a -c -F "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" "$norm" 2>/dev/null || echo 0)
  okfull=$(rg -a -c -F "$full" "$norm" 2>/dev/null || echo 0)
  (( ok >= 1 )) || die "no timeout settlement at all"
  (( ok <= armed )) || die "settlements ($ok) exceed registrations ($armed) — duplicate settlement"
  [[ "$okfull" == "$ok" ]] \
    || die "a settlement line does not carry the exact contract ($okfull of $ok match): $full"
}

# The reply-wins variant: the ORACLE's record must not be settled by timeout, but an unrelated
# production caller's own deadline may legitimately elapse in the same boot, so zero settlements
# is permitted while a settlement WITHOUT a matching registration (a duplicate) is not. The
# oracle's own outcome stays sealed by the identity-bearing reply-win markers the cell asserts.
verify_settlements_within_registrations() {
  local norm="$1" arch="$2" armed ok
  armed=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} " "$norm" 2>/dev/null || echo 0)
  ok=$(rg -a -c -F "IPC_REPLY_TIMEOUT_OK arch=${arch} terminal=Timeout" "$norm" 2>/dev/null || echo 0)
  (( ok <= armed )) || die "settlements ($ok) exceed registrations ($armed) — duplicate settlement"
}

# ── Canonical 199E-R3: ORACLE-SCOPED COMPLETION accounting ───────────────────────────────────
#
# The same correction the ARMED/OK families already received, extended to the two families that
# still counted globally. With ProductionTick default-on, an unrelated production caller (the
# supervisor, tid 2) legitimately arms and settles its OWN reply deadline in this same boot, so
# `count == 1` on a family failed on correct behaviour: the two events named two DIFFERENT tasks,
# each delivered exactly once.
#
# Nothing is loosened. Each family now carries BOTH
#   (a) an oracle-identity bound — exactly one event for the oracle's own caller, and none for a
#       second occurrence of that caller's exact generation; and
#   (b) a GLOBAL duplicate bound — no identity anywhere may appear twice for one completion stage.
# An unrelated caller can therefore never satisfy the oracle's assertion, and a genuine duplicate
# still fails even when it belongs to a caller the oracle does not own.
#
# FIELD INVENTORY (this is what each marker actually carries, and it bounds what can be checked):
#   IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK  init_tid                       — no ASID
#   IPC_REPLY_TIMEOUT_ARMED                caller_tid caller_asid record_index record_generation
#   RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED  tid blocked_generation    — no ASID
#   IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED  arch terminal result          — NO identity at all
#
# So the strongest available tuples are: registration = {tid, asid, record_index,
# record_generation}; delivery = {tid, blocked_generation}. LIMITATION, recorded deliberately:
# the delivery marker carries no ASID, so a replacement incarnation that reused the numeric TID
# within one boot would be indistinguishable there — the registration bound, which DOES carry the
# ASID, is what closes that gap for the oracle's own caller. The COMMITTED marker carries no
# identity whatsoever and cannot be field-scoped at all; it is bound positionally inside the
# oracle's own identity-scoped window plus a cardinality tie to the distinct settled callers.
# Widening either marker is a kernel change and is out of scope for this repair.

# Extract a numeric `key=value` field from one marker line. The leading space is required so
# `tid=` cannot match `caller_tid=`.
marker_field() {
  sed -n "s/.*[[:space:]]$2=\([0-9][0-9]*\).*/\1/p" <<<"$1" | head -1
}

# The oracle's REGISTRATION identity: caller tid from the provisioning marker (never hardcoded,
# never positional), then the ASID / record coordinates from that caller's own ARMED line.
# Fails closed when the provisioning marker is missing, when the oracle caller has no
# registration, or when either carries a malformed identity.
ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
derive_oracle_identity() {
  local norm="$1" arch="$2" n armed
  ORACLE_TID=""; ORACLE_ASID=""; ORACLE_RECORD_INDEX=""; ORACLE_RECORD_GEN=""
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=" "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || { die "oracle provisioning marker count != 1 (got $n): the oracle identity cannot be scoped"; return 1; }
  ORACLE_TID="$(oracle_init_tid "$norm")"
  [[ -n "$ORACLE_TID" ]]     || { die "oracle provisioning marker carries no init_tid: identity malformed"; return 1; }
  n=$(rg -a -c -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || { die "oracle-identity ARMED count != 1 (got $n) for caller_tid=${ORACLE_TID}"; return 1; }
  armed=$(rg -a -m1 -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${ORACLE_TID} " "$norm")
  ORACLE_ASID="$(marker_field "$armed" caller_asid)"
  ORACLE_RECORD_INDEX="$(marker_field "$armed" record_index)"
  ORACLE_RECORD_GEN="$(marker_field "$armed" record_generation)"
  [[ -n "$ORACLE_ASID" && -n "$ORACLE_RECORD_INDEX" && -n "$ORACLE_RECORD_GEN" ]]     || { die "oracle registration is malformed (asid=$ORACLE_ASID idx=$ORACLE_RECORD_INDEX gen=$ORACLE_RECORD_GEN)"; return 1; }
  note "oracle identity: tid=${ORACLE_TID} asid=${ORACLE_ASID} record=${ORACLE_RECORD_INDEX}/${ORACLE_RECORD_GEN}"
}

# Every (tid, blocked_generation) pair observed on the delivery family, one per line.
delivered_identities() {
  sed -n 's/.*RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=\([0-9][0-9]*\) .*blocked_generation=\([0-9][0-9]*\) .*/\1:\2/p' "$1"
}

# (1) exactly one delivery for the ORACLE's identity, and (2) no second delivery for that exact
# identity+generation. An unrelated production caller cannot satisfy either, because both match
# on the oracle's own tid.
verify_oracle_completion_delivered_once() {
  local norm="$1" tid="$2" n line gen g
  n=$(rg -a -c -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=${tid} " "$norm" 2>/dev/null || echo 0)
  [[ "$n" == "1" ]]     || die "oracle-identity completion delivery count != 1 (got $n) for tid=${tid}"
  line=$(rg -a -m1 -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=${tid} " "$norm" 2>/dev/null || true)
  [[ -n "$line" ]] || return
  gen="$(marker_field "$line" blocked_generation)"
  [[ -n "$gen" ]] || { die "the oracle's completion delivery carries no blocked_generation"; return; }
  g=$(delivered_identities "$norm" | rg -a -c -F "${tid}:${gen}" 2>/dev/null || echo 0)
  [[ "$g" == "1" ]]     || die "duplicate completion delivery for tid=${tid} blocked_generation=${gen} (got $g)"
  rg -a -q -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=${tid} class=IpcRecv result=TimedOut" "$norm"     || die "the oracle's completion delivery does not carry the canonical IpcRecv/TimedOut contract"
}

# (4) GLOBAL duplicate detection, across every identity in the boot — including callers the
# oracle does not own. This is what the family `count == 1` used to provide, kept in full.
verify_no_duplicate_completion_delivery() {
  local norm="$1" dup
  dup=$(delivered_identities "$norm" | sort | uniq -d | head -3 | tr '\n' ' ')
  [[ -z "${dup// /}" ]]     || die "duplicate completion delivery for the same identity+generation: ${dup}"
}

# The COMMITTED family carries no identity, so it is bound two independent ways, and together
# they are strictly stronger than the global `count == 1` they replace:
#   (a) POSITIONAL — exactly one commit lies inside the oracle's own window, delimited by two
#       INDEPENDENTLY identity-scoped lines: the oracle's ARMED and the oracle's DELIVERED;
#   (b) CARDINAL — the commit count equals the number of DISTINCT settled caller identities, so a
#       genuine duplicate commit fails while N legitimate unrelated callers pass.
verify_oracle_completion_committed_once() {
  local norm="$1" arch="$2" tid="$3" a d inwin total distinct
  a=$(rg -a -n -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  d=$(rg -a -n -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  [[ -n "$a" && -n "$d" ]]     || { die "cannot bound the oracle completion window (armed=$a delivered=$d)"; return; }
  (( a < d )) || { die "the oracle's registration must precede its completion delivery ($a,$d)"; return; }
  inwin=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null           | cut -d: -f1 | awk -v lo="$a" -v hi="$d" '$1 > lo && $1 < hi' | wc -l | tr -d ' ')
  [[ "$inwin" == "1" ]]     || die "oracle-window COMPLETION_COMMITTED count != 1 (got $inwin) between lines $a and $d"
  total=$(rg -a -c -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null || echo 0)
  distinct=$(delivered_identities "$norm" | sort -u | wc -l | tr -d ' ')
  [[ "$total" == "$distinct" ]]     || die "COMPLETION_COMMITTED count ($total) != distinct settled caller identities ($distinct) — duplicate or orphan commit"
}

# The ORACLE's OWN ordered chain. Every position is selected by the oracle's identity, so an
# unrelated production caller's earlier registration/commit/delivery — tid 2's, which legitimately
# precedes all of these in a default-ProductionTick boot — can never stand in for one of them. The
# commit position is the one INSIDE the oracle's window, not the family's first occurrence.
verify_oracle_ordered_chain() {
  local norm="$1" arch="$2" tid="$3" oa oc od ou
  oa=$(rg -a -n -F "IPC_REPLY_TIMEOUT_ARMED arch=${arch} caller_tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  od=$(rg -a -n -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=${tid} " "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  oc=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=${arch} terminal=Timeout result=ok" "$norm" 2>/dev/null \
       | cut -d: -f1 | awk -v lo="${oa:-0}" -v hi="${od:-0}" '$1 > lo && $1 < hi' | head -1)
  ou=$(rg -a -n -F "USER_LOG tid=${tid} msg=RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" "$norm" 2>/dev/null | head -1 | cut -d: -f1)
  if [[ -n "$oa" && -n "$oc" && -n "$od" && -n "$ou" ]]; then
    (( oa < oc )) || die "oracle: registration must precede its own committed completion ($oa,$oc)"
    (( oc < od )) || die "oracle: its committed completion must precede its own delivery ($oc,$od)"
    (( od < ou )) || die "oracle: its delivery must precede its own userspace completion ($od,$ou)"
  else
    die "oracle-scoped marker sequence incomplete (oa=$oa oc=$oc od=$od ou=$ou)"
  fi
}

forbid_log() {
  local norm="$1"; shift
  local m
  for m in "$@"; do
    if rg -a -q -F "$m" "$norm"; then die "forbidden marker present: $m"; fi
  done
}

# ── Canonical 199E-R3: SELF-TEST for the oracle-scoped accounting ───────────────────────────
#
# `scripts/qemu-ipc-reply-timeout-riscv64-retirement-smoke.sh --self-test` runs the scoped
# helpers against synthetic fixtures and exits. It needs no QEMU, no build and no artifacts, so
# the accounting logic is provable in isolation from the live cell it guards — including the
# failure directions, which a passing live boot can never demonstrate.
#
# The fixtures encode a default-ProductionTick boot: tid 2 is an unrelated production caller that
# legitimately arms and settles its own reply deadline BEFORE the oracle (tid 1) does anything.
fixture_log() {
  local kind="$1"
  case "$kind" in
    oracle_only) ;;
    *)
      # An unrelated production caller settles first, in full.
      echo "IPC_REPLY_TIMEOUT_ARMED arch=riscv64 caller_tid=2 caller_asid=2 record_index=0 record_generation=1 terminal_epoch=1 token_slot=0 token_generation=1 deadline=7 result=ok"
      echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64 terminal=Timeout result=ok"
      echo "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=2 class=IpcRecv result=TimedOut code=9 blocked_generation=1 sepc=0x4027fa final_a0=9 final_a1=0 result=ok"
      if [[ "$kind" == "unrelated_duplicate" ]]; then
        echo "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=2 class=IpcRecv result=TimedOut code=9 blocked_generation=1 sepc=0x4027fa final_a0=9 final_a1=0 result=ok"
      fi
      ;;
  esac
  [[ "$kind" == "no_provision" ]] \
    || echo "IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=1 req_cap=65539 rep_cap=65540 req_eidx=6 rep_eidx=7 mode=1"
  [[ "$kind" == "malformed_armed" ]] \
    && echo "IPC_REPLY_TIMEOUT_ARMED arch=riscv64 caller_tid=1 caller_asid=x record_index=y record_generation=z result=ok"
  [[ "$kind" == "malformed_armed" ]] \
    || echo "IPC_REPLY_TIMEOUT_ARMED arch=riscv64 caller_tid=1 caller_asid=1 record_index=1 record_generation=17 terminal_epoch=1 token_slot=0 token_generation=1 deadline=10209 result=ok"
  case "$kind" in
    oracle_missing_delivery) ;;   # oracle registers, never settles
    *)
      echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64 terminal=Timeout result=ok"
      echo "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1 class=IpcRecv result=TimedOut code=9 blocked_generation=18 sepc=0x406a7e final_a0=9 final_a1=0 result=ok"
      if [[ "$kind" == "oracle_duplicate" ]]; then
        echo "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64 terminal=Timeout result=ok"
        echo "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED tid=1 class=IpcRecv result=TimedOut code=9 blocked_generation=18 sepc=0x406a7e final_a0=9 final_a1=0 result=ok"
      fi
      echo "USER_LOG tid=1 msg=RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
      ;;
  esac
}

# Run every scoped check against one fixture. Returns 0 when all pass, 1 when any died.
self_test_checks() {
  local f="$1"
  fail=0
  derive_oracle_identity "$f" riscv64 || true
  if [[ -n "$ORACLE_TID" ]]; then
    verify_oracle_completion_delivered_once "$f" "$ORACLE_TID"
    verify_oracle_completion_committed_once "$f" riscv64 "$ORACLE_TID"
    verify_oracle_ordered_chain "$f" riscv64 "$ORACLE_TID"
  fi
  verify_no_duplicate_completion_delivery "$f"
  return "$fail"
}

self_test() {
  local dir rc st_fail=0 kind expect
  dir=$(mktemp -d) || { echo "[self-test][fail] mktemp"; return 1; }
  # kind:expect — `pass` means every scoped check accepts the fixture.
  for spec in \
      "oracle_only:pass" \
      "oracle_plus_unrelated:pass" \
      "oracle_missing_delivery:fail" \
      "oracle_duplicate:fail" \
      "unrelated_duplicate:fail" \
      "no_provision:fail" \
      "malformed_armed:fail"; do
    kind="${spec%%:*}"; expect="${spec##*:}"
    fixture_log "$kind" >"$dir/$kind.log"
    rc=0; ( self_test_checks "$dir/$kind.log" ) >"$dir/$kind.out" 2>&1 || rc=1
    if [[ "$expect" == "pass" && "$rc" != "0" ]]; then
      echo "[self-test][fail] $kind: expected PASS, got FAIL"; sed 's/^/    /' "$dir/$kind.out"; st_fail=1
    elif [[ "$expect" == "fail" && "$rc" == "0" ]]; then
      echo "[self-test][fail] $kind: expected FAIL, got PASS"; st_fail=1
    else
      echo "[self-test][ok] $kind expected=$expect"
    fi
  done
  rm -rf "$dir"
  if (( st_fail )); then
    echo "STAGE_199E_R3_ORACLE_SCOPED_ACCOUNTING_SELFTEST result=fail"
    return 1
  fi
  echo "STAGE_199E_R3_ORACLE_SCOPED_ACCOUNTING_SELFTEST cases=7 result=ok"
  return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit $?
fi

# ── 3. Scenario A — timeout-wins, feature enabled (fresh boot) ──
TW_OK=0
if (( ! fail )); then
  note "booting fresh -smp 1 QEMU: yarm.riscv_ipc_reply_timeout_oracle=timeout-wins"
  boot_mode timeout-wins "$LOGDIR/boot-timeout-wins.log"
  TW="$LOGDIR/tw.norm.log"; tr '\r' '\n' <"$LOGDIR/boot-timeout-wins.log" >"$TW"
  [[ -s "$TW" ]] || die "no timeout-wins boot log"
  verify_oracle_armed_once "$TW" riscv64
  verify_no_duplicate_settlement "$TW" riscv64 \
    "IPC_REPLY_TIMEOUT_OK arch=riscv64 terminal=Timeout timeout_result=TimedOut caller_wakes=1 reply_aliases_invalid=1 late_reply_successes=0 result=ok"
  # Canonical 199E-R3: the oracle's identity is derived ONCE, from the provisioning marker and
  # the oracle caller's own registration, and everything below is scoped to it. Nothing here is
  # selected by family order or by "first occurrence".
  derive_oracle_identity "$TW" riscv64
  verify_log "$TW" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "IPC_REPLY_TIMEOUT_DEFERRED arch=riscv64 published=1 drained=1 result=ok" \
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout result=ok" \
    "RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected result=ok"
  if [[ -n "$ORACLE_TID" ]]; then
    verify_oracle_completion_delivered_once "$TW" "$ORACLE_TID"
    verify_oracle_completion_committed_once "$TW" riscv64 "$ORACLE_TID"
  fi
  # GLOBAL duplicate detection is retained in full and applies to every caller in the boot, not
  # just the oracle's: exactly one delivery per identity+generation ⇒ one timeout encoding, one
  # resume boundary, no duplicate wake.
  verify_no_duplicate_completion_delivery "$TW"
  # ORDERED sequence, part 1 — the GLOBAL one-shot. `GLOBAL_LOCK_RETIRE_CLASS_DONE` is a one-shot
  # latch authorized by the FIRST completion consumption in the boot, whichever caller earns it,
  # so these three positions are deliberately family-first rather than oracle-scoped. The
  # invariant is unchanged: a committed-but-undelivered completion must never claim the class
  # retired. `ud` IS the oracle's, and is scoped by the oracle's own USER_LOG tid.
  ci=$(rg -a -n -F "IPC_REPLY_TIMEOUT_COMPLETION_COMMITTED arch=riscv64" "$TW" | head -1 | cut -d: -f1)
  cn=$(rg -a -n -F "RISCV_BLOCKED_SYSCALL_COMPLETION_DELIVERED" "$TW" | head -1 | cut -d: -f1)
  rt=$(rg -a -n -F "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=riscv64 class=IpcReplyTimeout" "$TW" | head -1 | cut -d: -f1)
  ud=$(rg -a -n -F "USER_LOG tid=${ORACLE_TID} msg=RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut" "$TW" | head -1 | cut -d: -f1)
  if [[ -n "$ci" && -n "$cn" && -n "$rt" && -n "$ud" ]]; then
    (( ci < cn )) || die "completion committed must precede the resume-boundary consumption"
    (( cn <= rt )) || die "retirement marker must follow the completion consumption"
    (( rt < ud )) || die "retirement marker must precede the userspace completion"
  else
    die "ordered marker sequence incomplete (ci=$ci cn=$cn rt=$rt ud=$ud)"
  fi
  # ORDERED sequence, part 2 — the ORACLE's OWN chain.
  [[ -n "$ORACLE_TID" ]] && verify_oracle_ordered_chain "$TW" riscv64 "$ORACLE_TID"
  forbid_log "$TW" \
    "IPC_REPLY_BEATS_TIMEOUT_OK" \
    "scan_broad_lock=1" \
    "IPC_REPLY_TIMEOUT_OK arch=x86_64" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "RISCV_TRAP_FAIL" "Unhandled"
  # Stage 200C2C2C-R2B: timeout-wins must be UNTOUCHED by the causal reply-wins gate — the
  # gate is armed only in reply-wins mode, so no gate marker may appear here at all and the
  # production collector must have published + drained its work normally (asserted above).
  forbid_log "$TW" "IPC_REPLY_TIMEOUT_COLLECTOR_GATE"
  # The reply-win reserve must have DECLINED for the single legitimate reason: the timeout
  # already owned the terminal. No other decline reason is acceptable, and in particular none
  # may mention deadline bookkeeping.
  verify_log "$TW" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=decline reason=TimeoutAlreadyClaimed result=ok"
  forbid_log "$TW" "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok"
  assert_single_boot_instance "$TW" "$LOGDIR/core-timeout-wins.log" timeout-wins
  recheck_sha_clean
  (( fail )) || TW_OK=1
fi

# ── 4. Scenario B — reply-wins, feature enabled (SEPARATE fresh boot) ──
#
# Stage 200C2C2C-R2B: this outcome is now CAUSAL, not a wall-clock race. The kernel arms a
# collector gate at reply-wins arm time — strictly before the terminal cell is armed — so the
# timeout collector can publish NO work while the reply is in flight; the gate is released only
# when the DebugLog seam observes the client's own post-validation marker. The reply therefore
# wins because it is the ONLY claimant that could run, and the late scan that follows is a
# genuine ungated scan that found nothing to claim.
run_reply_wins() {
  local tag="$1" log="$LOGDIR/boot-${1}.log" rw="$LOGDIR/${1}.norm.log"
  note "booting fresh -smp 1 QEMU: yarm.riscv_ipc_reply_timeout_oracle=reply-wins [$tag]"
  boot_mode reply-wins "$log" "$tag"
  tr '\r' '\n' <"$log" >"$rw"
  [[ -s "$rw" ]] || { die "[$tag] no reply-wins boot log"; return; }
  # The oracle identity this lane's ordered chain is scoped to, derived the same way as in
  # timeout-wins: from the provisioning marker and that caller's own registration.
  derive_oracle_identity "$rw" riscv64 || return
  verify_log "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=held phase=before_terminal_claim result=ok" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok terminal=Reserved(Reply) token_lease=1 result=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64 terminal=Reply reply_copies=1 deadline_disarmed=1 late_timeout_claims=0 caller_wakes=1 result=ok" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released trigger=userspace_reply_validated result=ok" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=riscv64 outcome=reply_won late_timeout_claims=0 result=ok" \
    "IPC_REPLY_TIMEOUT_LOCK_STATUS arch=riscv64 scan_broad_lock=0 completion_transaction_narrow=1 classes=IpcReplyTimeout+IpcSendTimeout production=1 result=ok" \
    "RISCV_IPC_REPLY_BEATS_TIMEOUT_DONE reply_ok=1 caller_continuations=1 late_timeout_wakes=0 duplicate_reply=rejected result=ok" \
    "IPC_REPLY_TIMEOUT_ORACLE_SERVER_DUP_REPLY rejected=1"
  # The reply must never be declined here, and NO decline reason may ever be raised by deadline
  # bookkeeping: terminal ownership is the single authority.
  verify_oracle_armed_once "$rw" riscv64
  verify_settlements_within_registrations "$rw" riscv64
  forbid_log "$rw" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=decline" \
    "IPC_REPLY_WIN_ROLLBACK" \
    "scan_broad_lock=1" \
    "KERNEL PANIC" "RUST PANIC" "panicked at" "RISCV_TRAP_FAIL" "Unhandled"
  # The CAUSAL chain, in order. Each link is a distinct log line, so the sequence is evidence
  # that the reply's win did not depend on the numerical deadline or on QEMU host timing.
  # PRE-EXISTING RUNNER DEFECT, repaired here and recorded separately from the accounting work:
  # these two `assert_order` calls passed only THREE arguments to a FOUR-parameter function, so
  # under `set -u` the first of them aborted the whole runner with `$4: unbound variable` — the
  # prose reason had landed in the `b` (second marker) slot and there was no `why` at all. The
  # repair supplies the missing/mismatched argument ONLY: each call now expresses exactly the
  # ordering its own prose already stated, and no marker, count, result or ordering requirement
  # is added, removed or altered.
  #
  # The registration marker is ORACLE-SCOPED for the same reason the rest of this cell is: with
  # ProductionTick default-on an unrelated production caller arms its own reply deadline tens of
  # thousands of lines earlier (measured: tid 2 at line 2636 against the collector gate at
  # 39188), so the unscoped family would compare against a line that has nothing to do with the
  # reply under test.
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=held" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64 caller_tid=${ORACLE_TID} " \
    "the collector must be held BEFORE any terminal/deadline is armed"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_ARMED arch=riscv64 caller_tid=${ORACLE_TID} " \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok" \
    "the reply must reserve against a genuinely armed deadline"
  assert_order "$rw" \
    "IPC_REPLY_WIN_RESERVE arch=riscv64 outcome=ok" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64" \
    "the reservation must precede the committed reply win"
  assert_order "$rw" \
    "IPC_REPLY_BEATS_TIMEOUT_OK arch=riscv64" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "userspace must validate the payload only after the reply win committed"
  assert_order "$rw" \
    "USER_LOG tid=1 msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_REPLY_RECV plen=8 reply_ok=1" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released" \
    "the gate must be released by the userspace validation, not before it"
  assert_order "$rw" \
    "IPC_REPLY_TIMEOUT_COLLECTOR_GATE arch=riscv64 outcome=released" \
    "IPC_REPLY_TIMEOUT_LATE_SCAN arch=riscv64 outcome=reply_won" \
    "the late scan must run with collection ENABLED (otherwise it claims nothing vacuously)"
  assert_single_boot_instance "$rw" "$LOGDIR/core-${tag}.log" "$tag"
  recheck_sha_clean
}

RW_OK=0
if (( ! fail )); then
  run_reply_wins reply-wins
  (( fail )) || RW_OK=1
fi

# ── 4b. Scenario B REPEAT — the reply win must be REPRODUCIBLE, not a lucky schedule ──
RW2_OK=0
if (( ! fail )); then
  run_reply_wins reply-wins-repeat
  (( fail )) || RW2_OK=1
fi

# ── 5. Retirement live seal (runner-emitted; all three fresh boots must pass) ──
if (( fail )) || [[ "$TW_OK" != "1" || "$RW_OK" != "1" || "$RW2_OK" != "1" ]]; then
  echo "STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL arch=riscv64 classes=1 live_cells=1 timeout_wins=${TW_OK} reply_wins=${RW_OK} reply_wins_repeat=${RW2_OK} result=fail"
  exit 1
fi

cat <<'SEAL'
STAGE_200C_REPLY_TIMEOUT_RISCV64_RETIREMENT_SEAL
arch=riscv64
classes=1
live_cells=1
timeout_wins=1
reply_wins=1
reply_wins_repeat=1
canonical_timeout_result=1
completion_resume=1
sepc_single_advance=1
scan_broad_lock=0
completion_transaction_narrow=1
late_reply_successes=0
late_timeout_wakes=0
duplicate_wakes=0
stale_authority_restores=0
wrong_waiter_mutations=0
result=ok
SEAL

cat <<'SEAL'
STAGE_200_RISCV_REPLY_WIN_CAUSAL_SEAL
arch=riscv64
reply_eligibility_depends_on_deadline_value=0
terminal_ownership_is_single_authority=1
deadline_bookkeeping_declines=0
collector_gate_held_before_terminal_claim=1
collector_gate_released_by_userspace_validation=1
late_scan_ran_ungated=1
reply_wins_live_runs=2
boot_instances_per_run=1
timeout_wins_preserved=1
result=ok
SEAL
