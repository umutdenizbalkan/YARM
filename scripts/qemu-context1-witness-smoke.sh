#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-CONTEXT1 — the user execution-state witness, one architecture per run:
#
#   scripts/qemu-context1-witness-smoke.sh x86_64|aarch64
#
#   LOGDIR=...        build/boot logs (default /tmp/qemu-context1-witness-<arch>)
#   FEATURES=...      kernel/server features (default: context1-witness,context1-clobber)
#   SKIP_BUILD=1      reuse $LOGDIR/build
#   REGRADE=1         grade $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS=...  boot bound (default 420)
#   QUANTUM=...       yarm.sched_quantum_ticks (default 2). The shipped quantum is the hardware
#                     deadline constant in interrupt units (~50M on x86_64), so without this
#                     existing qualification knob no timer ever preempts inside a boot.
#
# One -smp 1 boot (the architecture's core-smoke machine at one CPU) of a build carrying init's
# witness cell (selector 31) and the kernel's CTX1_TRAP identity markers; with
# `context1-clobber` the kernel also overwrites every user-visible FP/SIMD/control register after
# each user-origin trap, so nothing survives by luck. The boot ends when the witness summary
# appears (plus a short tail) or at the bound. This script grades; it never retries.
#
# Graded, each separately, for every window:
#   * STATE: the witness's own full-image comparison (mask=0x0, flags_bad=0, gpr_bad=0x0);
#   * IDENTITY / TRANSITIONS, from the kernel's CTX1_TRAP lines inside the window:
#       fresh   — A blocked and C (a new thread) was entered;
#       block   — A blocked (in=A out=0), the CPU idled, and an idle-origin trap resumed A;
#       preempt — a timer tick switched A -> B (in=A out=B), and the first return from B to A
#                 took the route the round's mode names — a timer tick (spin) or B's blocking
#                 syscall (block) — with B's own "B ran" store observed by A (b_ran=1) and A's
#                 six syscall-lane GPR sentinels intact (gpr_bad=0x0); both routes required;
#       same    — at least one timer tick interrupted A in user mode and returned to A;
#   * B's own windows all intact; the kernel ran under its own FP environment on every user
#     entry (CTX1_KERNEL_ENV_BAD absent); nothing fatal.
set -uo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-x86_64}
case "$ARCH" in x86_64|aarch64) ;; *) echo "usage: $0 x86_64|aarch64"; exit 2;; esac
LOGDIR=${LOGDIR:-/tmp/qemu-context1-witness-$ARCH}
FEATURES=${FEATURES:-context1-witness,context1-clobber}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[context1-witness] building $ARCH with $FEATURES"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features $FEATURES" \
    "scripts/build-qemu-$ARCH-artifacts.sh" >"$LOGDIR/build.log" 2>&1 \
    || { echo "CONTEXT1_WITNESS_SEAL arch=$ARCH result=fail reason=build"; exit 1; }
fi

if [[ "${REGRADE:-0}" != "1" ]]; then
  rm -f "$BOOT_LOG"
  if [[ "$ARCH" == x86_64 ]]; then
    QEMU=(qemu-system-x86_64 -machine q35 -cpu qemu64 -m 512M -smp 1
          -kernel "$BUILD_DIR/kernel_boot.elf" -initrd "$BUILD_DIR/initramfs-core.cpio"
          -append "console=ttyS0 rdinit=/init yarm.sched_quantum_ticks=${QUANTUM:-2}")
  else
    QEMU=(qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 1024M -smp 1
          -kernel "$BUILD_DIR/yarm-aarch64.bin" -initrd "$BUILD_DIR/initramfs-core.cpio"
          -append "yarm.sched_quantum_ticks=${QUANTUM:-2}")
  fi
  echo "[context1-witness] booting: ${QEMU[*]}"
  echo "#HOST_QEMU ${QEMU[*]} $(${QEMU[0]} --version | head -1)" >"$BOOT_LOG.host"
  "${QEMU[@]}" -display none -monitor none -no-reboot -serial "file:$BOOT_LOG" &
  qpid=$!
  deadline=$((SECONDS + ${TIMEOUT_SECS:-420}))
  seen=""
  while kill -0 "$qpid" 2>/dev/null && (( SECONDS < deadline )); do
    if [[ -z "$seen" ]] && grep -a -q "CTX1_WITNESS arch=" "$BOOT_LOG" 2>/dev/null; then
      seen=$SECONDS
    fi
    if [[ -n "$seen" ]] && (( SECONDS - seen >= 10 )); then break; fi
    sleep 1
  done
  kill "$qpid" 2>/dev/null; wait "$qpid" 2>/dev/null
  [[ -n "$seen" ]] || echo "#HOST_TIMEOUT" >>"$BOOT_LOG.host"
fi
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"

python3 - "$NORM" "$ARCH" <<'PY'
import re, sys
path, arch = sys.argv[1], sys.argv[2]
lines = open(path, errors='replace').read().split('\n')
fails = []
def fail(msg): fails.append(msg); print(f"[context1-witness][fail] {msg}")

def kv(line):
    return dict(re.findall(r'(\w+)=([^\s]+)', line))

summ = [l for l in lines if 'CTX1_WITNESS arch=' in l]
begin = [l for l in lines if 'CTX1_WITNESS_BEGIN' in l]
if len(summ) != 1 or len(begin) != 1:
    fail(f"expected one witness begin and summary, got {len(begin)}/{len(summ)}")
    print(f"CONTEXT1_WITNESS_SEAL arch={arch} result=fail reason=incomplete")
    sys.exit(1)
S = kv(summ[0][summ[0].index('CTX1_WITNESS arch='):])
A, B, C = S.get('a_tid'), S.get('b_tid'), S.get('c_tid')
if S.get('result') != 'ok' or S.get('failures') != '0':
    fail(f"witness summary: {summ[0].strip()[-160:]}")

traps = []  # (index, dict)
for i, l in enumerate(lines):
    if 'CTX1_TRAP ' in l:
        traps.append((i, kv(l[l.index('CTX1_TRAP '):])))

def windows(cell):
    out = []
    for i, l in enumerate(lines):
        if f'CTX1_WINDOW cell={cell} ' in l and l.rstrip().endswith('begin'):
            r = kv(l[l.index('CTX1_WINDOW'):]).get('round')
            for j in range(i + 1, len(lines)):
                if f'CTX1_RESULT cell={cell} round={r} ' in lines[j]:
                    out.append((r, i, j, kv(lines[j][lines[j].index('CTX1_RESULT'):])))
                    break
    return out

def traps_in(i, j):
    return [t for k, t in traps if i < k < j]

counts = {}
# fresh
for r, i, j, R in windows('fresh'):
    tr = traps_in(i, j)
    entered_c = any(t['out'] == C for t in tr)
    a_left = any(t['in'] == A and t['out'] != A for t in tr)
    if R.get('result') != 'ok': fail(f"fresh: {R}")
    if not (entered_c and a_left): fail(f"fresh: no A-block then C-entry in window (A left={a_left} C entered={entered_c})")
    counts['fresh'] = counts.get('fresh', 0) + (R.get('result') == 'ok' and entered_c and a_left)
# block
for r, i, j, R in windows('block'):
    tr = traps_in(i, j)
    blk = [k for k, t in traps if i < k < j and t['in'] == A and t['out'] == '0']
    idle = any('SCHED_ENTER_IDLE_HLT' in lines[k] for k in range(i, j))
    res = [k for k, t in traps if i < k < j and t['origin'] == 'idle' and t['out'] == A]
    good = R.get('result') == 'ok' and blk and idle and res and min(blk) < max(res)
    if R.get('result') != 'ok': fail(f"block round {r}: {R}")
    if not (blk and idle and res): fail(f"block round {r}: block={len(blk)} idle={idle} idle_resume={len(res)}")
    counts['block'] = counts.get('block', 0) + bool(good)
# preempt: A -> B on a timer tick, then the FIRST return to A comes from B by the route the
# round's mode names: a timer tick (spin) or B's blocking syscall (block).
routes = {}
for r, i, j, R in windows('preempt'):
    tr = [(k, t) for k, t in traps if i < k < j]
    out_ab = [k for k, t in tr if t['in'] == A and t['out'] == B and t['timer'] == '1']
    back = [(k, t) for k, t in tr if out_ab and k > min(out_ab) and t['out'] == A and t['in'] == B]
    route = None
    if back:
        route = 'timer' if back[0][1]['timer'] == '1' else 'syscall'
    want = {'spin': 'timer', 'block': 'syscall'}.get(R.get('mode'))
    routes[r] = f"{R.get('mode')}:{route}"
    good = (R.get('result') == 'ok' and R.get('b_ran') == '1' and R.get('gpr_bad') == '0x0'
            and out_ab and back and route == want)
    if R.get('result') != 'ok': fail(f"preempt round {r}: {R}")
    if not (out_ab and back): fail(f"preempt round {r}: A->B switches={len(out_ab)} returns-to-A={len(back)}")
    elif route != want: fail(f"preempt round {r}: mode={R.get('mode')} returned to A by {route}, want {want}")
    counts['preempt'] = counts.get('preempt', 0) + bool(good)
modes = [v.split(':')[0] for v in routes.values()]
if 'spin' not in modes or 'block' not in modes: fail(f"preempt: both return routes required, got {routes}")
pb = [l for l in lines if 'CTX1_RESULT cell=preempt_b ' in l]
if len(pb) != 1 or 'result=ok' not in pb[0]: fail(f"B's own windows: {pb[-1].strip()[-140:] if pb else 'missing'}")
# same
for r, i, j, R in windows('same'):
    tr = traps_in(i, j)
    ticks = [t for t in tr if t['timer'] == '1' and t['origin'] == 'user' and t['in'] == A and t['out'] == A]
    good = R.get('result') == 'ok' and len(ticks) >= 1
    if R.get('result') != 'ok': fail(f"same round {r}: {R}")
    if not ticks: fail(f"same round {r}: no same-task user-origin timer tick inside the window")
    counts['same'] = counts.get('same', 0) + bool(good)

need = {'fresh': 1, 'block': 3, 'preempt': 3, 'same': 3}
for k, v in need.items():
    if counts.get(k, 0) != v: fail(f"{k}: {counts.get(k,0)}/{v} windows fully evidenced")
envbad = sum('CTX1_KERNEL_ENV_BAD' in l for l in lines)
if envbad: fail(f"kernel ran under a user FP environment ({envbad} logged violations)")
for bad in ('panicked at', 'KERNEL PANIC', 'x86 trap dispatch failed', 'YARM_AARCH64_TRAP_HANDLE failed',
            'AARCH64_DIRECT_DISPATCH_FATAL', 'USER_UNSUPPORTED_INSTRUCTION', 'PAGE_FAULT_UNHANDLED'):
    n = sum(bad in l for l in lines)
    if n: fail(f"{bad} x{n}")
first = [l.strip()[-200:] for l in lines if 'CTX1_RESULT' in l and 'result=fail' in l][:1]
print(f"[context1-witness] tids A={A} B={B} C={C}; windows {counts}; preempt routes {routes}; first failing result: {first[0] if first else 'none'}")
seal = 'ok' if not fails else 'fail'
print(f"CONTEXT1_WITNESS_SEAL arch={arch} a={A} b={B} c={C} fresh={counts.get('fresh',0)} block={counts.get('block',0)} preempt={counts.get('preempt',0)} same={counts.get('same',0)} env_bad={envbad} result={seal}")
sys.exit(0 if seal == 'ok' else 1)
PY
