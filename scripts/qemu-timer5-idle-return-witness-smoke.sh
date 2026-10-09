#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# U9-TIMER5 §3 — the IDLE-BOUNDARY RETURN witness, per architecture.
#
# Usage: scripts/qemu-timer5-idle-return-witness-smoke.sh [x86_64|aarch64|riscv64]
#
# All three ports. RISC-V joined in BL4a; its arm below records why it was refused before.
#
# The population under test is one timer interrupt taken while the CPU is parked at its kernel
# idle boundary AND the run queue is not empty. No ordinary boot produces it, and that was
# MEASURED rather than assumed — at the U9-TIMER5 base, with yarm.sched_quantum_ticks=1 forcing
# every tick to preempt so the branch could be reached at all:
#
#   * x86_64 reached the boundary 73 times and found an EMPTY queue every single time;
#   * AArch64 reached it ZERO times (14 irq_lower_a64 exceptions, no irq_current_spx at all),
#     because its idle loop halted with DAIF masked and a masked `wfi` wakes but never traps.
#
# So the witness manufactures the shape, on ONE CPU, out of production mechanisms only: init
# issues a BLOCKING IpcRecvTimeout on an endpoint nobody sends to, parks as the last runnable
# task, and the CPU reaches its halt loop. A later tick's off-lock timeout pipeline expires the
# deadline and makes init runnable; the NEXT tick finds the boundary with queued work, commits the
# queued-work settlement, and the bridge's drain selects and resumes exactly that task.
#
# What this asserts, in one clean boot per architecture:
#
#   * USERSPACE CAME BACK, every round: rounds=24 timed_out=24 delivered=0 regs_bad=0, so the
#     continuation, the canonical result and the callee-saved register file all survived. Marking
#     a task Running is explicitly NOT success here — the witness line is emitted by init, after
#     the resume, from userspace.
#   * THE BOUNDARY WAS GENUINELY ENTERED: at least one IDLE_BOUNDARY_PARK, and idle-boundary
#     advances for a substantial fraction of the rounds. Not every round is required to park — a
#     wake published while another trap is already in flight is legitimately resumed by that
#     trap's own drain — so the exact count is reported in the seal rather than forced.
#   * THE ROUTE SELECTED AND RESUMED: TIMER_IDLE_ADVANCE_DRAIN_DONE and the architecture's own
#     *_IDLE_BOUNDARY_USER_RETURN agree exactly — every advance that marked a task also converted
#     a hardware frame, and no conversion happened without an advance.
#   * NOTHING REACHED BROAD DISPATCH: reason=no_user_return_path is gone from the source, so its
#     count is zero by construction, and TIMER_IDLE_ADVANCE_BROAD_ENTRY (U9-TIMER2's census
#     marker) stays at zero.
#   * NO STACK ACCUMULATION: IDLE_BOUNDARY_PARK is emitted only when a park arrives DEEPER than
#     any before it on that CPU. A flat cycle settles after a handful; a cycle leaking one vector
#     frame per turn would report a new low every time. The bound is asserted against the number
#     of rounds, so "settles" is a claim about this boot rather than about the code.
#   * NO LOST WAKE: all 24 rounds completed. A wake lost between the queue check and the halt
#     would strand init forever and this would time out instead of passing quietly.
#
# Fails on: a missing, duplicated or failing witness line, a mismatch between advances and frame
# conversions, any broad arrival, per-round stack drift, or any fatal trap/panic/timeout.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
case "$ARCH" in
  x86_64)
    FEATURE=x86-shared-region-direct-oracle
    KTARGET=targets/x86_64-yarm-none.json
    KPROFILE=x86-none
    KELF=target/x86_64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-x86_64-artifacts.sh
    SMOKE=scripts/qemu-x86_64-core-smoke.sh
    INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio
    DEST=build-x86_64/kernel_boot.elf
    NEEDS_OBJCOPY=0
    RETURN_MARKER=X86_IDLE_BOUNDARY_USER_RETURN
    REGS_CHECKED=4
    SMP=1
    ;;
  aarch64)
    FEATURE=aarch64-shared-region-direct-oracle
    KTARGET=targets/aarch64-yarm-none.json
    KPROFILE=aarch64-none
    KELF=target/aarch64-yarm-none/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-aarch64-artifacts.sh
    SMOKE=scripts/qemu-aarch64-core-smoke.sh
    INITRAMFS_IMAGE=build-aarch64/initramfs-core.cpio
    DEST=build-aarch64/yarm-aarch64.bin
    NEEDS_OBJCOPY=1
    RETURN_MARKER=AARCH64_IDLE_BOUNDARY_USER_RETURN
    REGS_CHECKED=4
    # The AArch64 core profile boots -smp 2, and init's spawn chain depends on it: forced to
    # -smp 1 its `SpawnProcess` round trip returns `zero_pid` and `run()` returns before it ever
    # reaches the slot-5 cells, so the witness would silently not run. That is a property of this
    # profile's boot chain, not of the idle boundary — and the boundary is witnessed just as well
    # with two CPUs, since both park and either one's timer may perform the advance.
    SMP=2
    ;;
  riscv64)
    # BL4a — RISC-V is witnessed too. U9-TIMER5 refused it because only a PREEMPTING tick
    # examined a parked RISC-V CPU (~36 of ~1540 ticks per core boot), so a task whose deadline
    # expired while the CPU was parked waited for a preempting tick that mostly did not come
    # (measured then: 0 of 3 runs completed). Its idle boundary is the per-CPU latch QEMU-SMP3
    # rebuilt: armed only by an idle arrival (`reestablish_idle_boundary`) and required by every
    # accepted S-mode timer trap, so an S-origin timer IS the authenticated boundary.
    #
    # Its S-mode timer entry CONSTRUCTS the user return rather than converting a kernel frame:
    # the resume is `RISCV_S_MODE_TIMER_DISPATCH`, the park is the idle arrival's own marker, and
    # the wait is the stack-free `wfi` loop re-based through `sscratch` on every arrival, so there
    # is no low-water telemetry to assert and no stack to accumulate. Registers are not checked
    # on this port (`checked=0`, see init's cell).
    FEATURE=riscv-shared-region-direct-oracle
    KTARGET=riscv64gc-unknown-none-elf
    KPROFILE=release
    KELF=target/riscv64gc-unknown-none-elf/${KPROFILE}/kernel_boot
    BUILD_SCRIPT=scripts/build-qemu-riscv64-artifacts.sh
    SMOKE=scripts/qemu-riscv64-core-smoke.sh
    INITRAMFS_IMAGE=build-riscv64/initramfs-core.cpio
    DEST=build-riscv64/yarm-riscv64.bin
    NEEDS_OBJCOPY=1
    RETURN_MARKER=RISCV_S_MODE_TIMER_DISPATCH
    PARK_MARKER='RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io'
    STACK_ANCHOR=0
    REGS_CHECKED=0
    SMP=1
    ;;
  *) echo "[timer5-witness][fail] unknown arch: $ARCH"; exit 1 ;;
esac

# The converted ports park through the idle-boundary anchor, which reports each new low-water
# mark; RISC-V overrides both (see its arm above).
PARK_MARKER=${PARK_MARKER:-IDLE_BOUNDARY_PARK cpu=}
STACK_ANCHOR=${STACK_ANCHOR:-1}

BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/timer5-idle-return-witness-$ARCH}
TIMEOUT_SECS=${TIMEOUT_SECS:-${ARCH_TIMEOUT_SECS:-120}}
ROUNDS=${ROUNDS:-24}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[timer5-witness] $*"; }
die()  { echo "[timer5-witness][fail] $*"; fail=1; }

# The init cell is a CARGO feature as well as a runtime knob, and for a measured reason: init's
# address space runs at `AddressSpace::MAX_MAPPINGS` on the provisioned oracle profiles, and
# compiling this cell unconditionally cost one more mapping run — enough to make the XFER2 grant
# witness fail with `VM_FULL reason=mapping_bookkeeping_full max_mappings=128`. Feature-off builds
# carry none of its literals, so every other slot-5 profile keeps the headroom it had.
WITNESS_FEATURE=timer5-idle-return-witness
note "building base $ARCH artifacts with $WITNESS_FEATURE"
BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $WITNESS_FEATURE" \
  "$BUILD_SCRIPT" >"$LOGDIR/build.log" 2>&1 || die "base artifact build failed"

note "rebuilding kernel_boot with $FEATURE"
JSON_SPEC_ARG=()
[[ "$KTARGET" == *.json ]] && JSON_SPEC_ARG=(-Z json-target-spec)
cargo build --no-default-features --features "$FEATURE" \
  --target "$KTARGET" --profile "$KPROFILE" \
  -Z build-std="$BUILD_STD" "${JSON_SPEC_ARG[@]}" \
  -p yarm --bin kernel_boot \
  >"$LOGDIR/kbuild.log" 2>&1 || die "feature kernel build failed"

if [[ ! -f "$KELF" ]]; then
  die "feature kernel ELF missing"
elif (( NEEDS_OBJCOPY )); then
  if command -v llvm-objcopy >/dev/null 2>&1; then OBJCOPY=llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then OBJCOPY=rust-objcopy
  else OBJCOPY=""; die "no objcopy available to produce the raw kernel image"; fi
  [[ -n "$OBJCOPY" ]] && { "$OBJCOPY" -O binary "$KELF" "$DEST" >"$LOGDIR/objcopy.log" 2>&1 \
    || die "objcopy of the feature kernel failed"; }
else
  cp "$KELF" "$DEST" || die "feature kernel copy failed"
fi
KERNEL_IMAGE="$DEST"

if (( fail )); then
  echo "TIMER5_IDLE_RETURN_WITNESS_SEAL arch=$ARCH result=fail reason=build"
  exit 1
fi

# Every architecture runs on its SHIPPED quantum. No boot knob tunes the scheduler for this
# witness, and that is a property worth having rather than a convenience: U9-TIMER5 §2 made a
# non-preempting tick on a PARKED CPU advance too, so the boundary is examined every tick instead
# of once per quantum. Before that change the AArch64 profile took 74 ticks with not one
# preempting, and the witness could not run at all; forcing `yarm.sched_quantum_ticks=1` to make
# it run instead broke this profile's spawn round trip (`zero_pid`), which is a pre-existing
# fragility under maximal preemption and outside this package's scope.
note "booting QEMU -smp $SMP with yarm.timer5_idle_return_witness=1 (shipped quantum)"
env \
  KERNEL_IMAGE="$KERNEL_IMAGE" \
  INITRAMFS_IMAGE="$INITRAMFS_IMAGE" \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.timer5_idle_return_witness=1" \
  QEMU_SMP="$SMP" \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  "$SMOKE" >"$LOGDIR/core-smoke.log" 2>&1 || true

[[ -s "$BOOT_LOG" ]] || { echo "TIMER5_IDLE_RETURN_WITNESS_SEAL arch=$ARCH result=fail reason=no_boot_log"; exit 1; }
NORM="$LOGDIR/boot.norm.log"
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

count() { grep -a -F -- "$1" "$NORM" 2>/dev/null | wc -l | tr -d ' '; }

# ── (4) Userspace came back, every round ─────────────────────────────────────────────────────
n=$(count 'TIMER5_IDLE_RETURN_WITNESS rounds=')
[[ "$n" == "1" ]] || die "expected exactly one witness summary (got $n)"
line=$(grep -a -m1 -- 'TIMER5_IDLE_RETURN_WITNESS rounds=' "$NORM" || true)
note "witness: $line"
for prop in "rounds=$ROUNDS" "timed_out=$ROUNDS" "delivered=0" "regs_bad=0" \
            "regs_ok=$ROUNDS" "checked=$REGS_CHECKED" "result=ok"; do
  case "$line" in
    *" $prop"*) ;;
    *) die "witness summary missing $prop" ;;
  esac
done

# ── (1) The boundary was genuinely entered ───────────────────────────────────────────────────
#
# Asserted on the CHANGED ports only. RISC-V's idle primitive is `riscv_trap_halt`, which this
# package does not touch — it publishes no boundary because it needs none: its S-mode timer entry
# CONSTRUCTS a user return rather than converting a kernel frame, so there is no kernel frame whose
# redirection would have to be authorized. The RISC-V arm of this script is a regression check that
# the shared route still behaves, not the changed-port evidence.
parks=$(count "$PARK_MARKER")
if [[ -n "$RETURN_MARKER" ]]; then
  (( parks >= 1 )) || die "the CPU never reached its idle boundary"
fi

# ── (2)+(3) A timer made queued work dispatchable, and the route selected and resumed it ─────
# Not every round is required to park: a wake published while some other trap is already in
# flight is legitimately resumed by that trap's own drain, and the witness cannot control which.
# What must hold is that the idle-boundary path is doing real work rather than firing once by
# accident, so the bar is a substantial FRACTION of the rounds — and the exact count is reported
# in the seal rather than hidden behind a boolean.
MIN_ADVANCES=${MIN_ADVANCES:-$(( (ROUNDS + 3) / 4 ))}
advances=$(count 'TIMER_IDLE_ADVANCE_DRAIN_DONE')
dequeues=$(count 'TIMER_IDLE_ADVANCE_DEQUEUE_OK')

if [[ -n "$RETURN_MARKER" ]]; then
  (( advances >= MIN_ADVANCES )) \
    || die "expected at least $MIN_ADVANCES idle-boundary advances over $ROUNDS rounds (got $advances)"
  returns=$(count "$RETURN_MARKER")
  # BL4a: the boot keeps running until the smoke's timeout stops QEMU, so its final transaction can
  # be cut short. Exactly two shapes are evidence lost to the capture, and each discounts ONE
  # trailing advance, reported; every other advance must still match exactly:
  #   in_flight — the log's last dequeue follows the last DRAIN_DONE and nothing after it but that
  #               same advance's own TIMER_IDLE_ADVANCE_* steps (the final one possibly cut
  #               mid-line): the capture ended inside the advance, before its drain completed;
  #   cut_tail  — the log ends in a strict prefix of the return marker after the last DRAIN_DONE.
  read -r in_flight cut_tail < <(python3 -I - "$NORM" "$RETURN_MARKER" <<'PY'
import re, sys
lines=[l for l in open(sys.argv[1],errors='replace').read().split('\n')]
while lines and not lines[-1].strip(): lines.pop()
marker=sys.argv[2]
last=lines[-1].strip() if lines else ''
def last_index(pat):
    return max((i for i,l in enumerate(lines) if pat in l), default=-1)
dd=last_index('TIMER_IDLE_ADVANCE_DRAIN_DONE')
dq=last_index('TIMER_IDLE_ADVANCE_DEQUEUE_OK')
in_flight=0
if dq>dd and dq>=0:
    m=re.search(r'incoming=(\d+)', lines[dq])
    tid=m.group(1) if m else None
    rest=[l.strip() for l in lines[dq+1:] if l.strip()]
    def own(l, final):
        if final and 'TIMER_IDLE_ADVANCE_'.startswith(l):
            return True
        if not l.startswith('TIMER_IDLE_ADVANCE_') or 'DRAIN_DONE' in l or 'SETTLED' in l:
            return False
        return final or (tid is not None and re.search(r'incoming=%s(?!\d)' % tid, l) is not None)
    # The dequeue line itself may be the one the capture cut.
    in_flight=int(dq==len(lines)-1 or (tid is not None and all(own(l, i==len(rest)-1) for i,l in enumerate(rest))))
after=lines[dd+1:] if dd>=0 else []
cut=(not in_flight) and bool(after) and after[-1].strip()==last and marker.startswith(last) \
    and last!=marker and not any(marker in l for l in after)
print(in_flight, int(cut))
PY
)
  [[ "$dequeues" == "$(( advances + in_flight ))" ]] \
    || die "every advance must dequeue exactly one task ($dequeues vs $advances, in_flight=$in_flight)"
  [[ "$returns" == "$(( advances - cut_tail ))" ]] \
    || die "advances and frame conversions must agree ($advances vs $returns, cut_tail=$cut_tail)"
  (( in_flight == 0 )) || note "the capture ended inside the last advance, before its drain completed (discounted 1)"
  (( cut_tail == 0 )) || note "the capture ended inside the last advance's return marker (discounted 1)"
  refused=$(count 'IDLE_BOUNDARY_RETURN_REFUSED')
  [[ "$refused" == "0" ]] || die "a committed user return was refused at the frame ($refused)"
fi

# ── BL4a: no idle settlement strands a runnable task (RISC-V) ────────────────────────────────
#
# The S-mode timer path settles every non-dispatching tick as RISCV_S_MODE_TIMER_RESUME_IDLE and
# goes back to `wfi`; that marker records the queue depth it leaves behind. Before BL4a a deadline
# that expired on a NON-preempting idle tick left its task queued right there, waiting for the next
# preempting tick. Any such settlement with work queued is that stranded task.
if [[ "$ARCH" == "riscv64" ]]; then
  resume_idle=$(count 'RISCV_S_MODE_TIMER_RESUME_IDLE tick=')
  stranded=$(grep -a -E -- 'RISCV_S_MODE_TIMER_RESUME_IDLE tick=[0-9]+ runnable=[1-9]' "$NORM" | wc -l | tr -d ' ')
  untagged=$(grep -a -E -- 'RISCV_S_MODE_TIMER_RESUME_IDLE tick=[0-9]+$' "$NORM" | wc -l | tr -d ' ')
  nonpre=$(grep -a -F -- 'TIMER_SPLIT_IDLE_ADVANCE_COMMITTED' "$NORM" | grep -a -c -F -- 'preempt=0' || true)
  note "idle settlements: resume_idle=$resume_idle stranded=$stranded untagged=$untagged non_preempting_advances=$nonpre"
  [[ "$untagged" == "0" ]] || die "RISCV_S_MODE_TIMER_RESUME_IDLE lacks its queue depth ($untagged lines): wrong build"
  [[ "$stranded" == "0" ]] || die "an idle tick returned to wfi with a runnable task queued ($stranded times)"
  (( nonpre >= 1 )) || die "no non-preempting idle tick committed the advance"
fi

# ── Nothing reached broad dispatch ───────────────────────────────────────────────────────────
for banned in 'reason=no_user_return_path' 'TIMER_IDLE_ADVANCE_BROAD_ENTRY' \
              'TIMER_SPLIT_IDLE_ADVANCE_UNSUPPORTED'; do
  n=$(count "$banned")
  [[ "$n" == "0" ]] || die "$banned appeared $n times"
done

# ── No stack accumulation ────────────────────────────────────────────────────────────────────
#
# One IDLE_BOUNDARY_PARK line per NEW low-water mark. A cycle that leaked a vector frame per turn
# would report one per round; a flat cycle settles almost immediately. The bound is deliberately
# generous — a handful of distinct entry depths is normal, ROUNDS of them is the defect.
if (( STACK_ANCHOR )); then
  (( parks < ROUNDS )) \
    || die "the idle boundary kept arriving deeper: $parks new low-water marks over $ROUNDS rounds"
  note "stack anchor: $parks distinct low-water marks over $ROUNDS rounds"
  grep -a -m3 -- 'IDLE_BOUNDARY_PARK cpu=' "$NORM" || true
else
  note "idle arrivals: $parks (stack-free wait; no low-water telemetry on $ARCH)"
fi

# ── No fatal trap ────────────────────────────────────────────────────────────────────────────
for bad in 'KERNEL PANIC' 'panicked at' 'DISPATCH_FATAL' 'TERMINAL_FAULT_UNEXPECTED_DISPOSITION' \
           'TIMER_SPLIT_UNEXPECTED_DISPOSITION'; do
  n=$(count "$bad")
  [[ "$n" == "0" ]] || die "$bad appeared $n times"
done

if (( fail )); then
  echo "TIMER5_IDLE_RETURN_WITNESS_SEAL arch=$ARCH result=fail${stranded:+ stranded=$stranded}"
  exit 1
fi
echo "TIMER5_IDLE_RETURN_WITNESS_SEAL arch=$ARCH rounds=$ROUNDS advances=$advances parks=$parks result=ok"
