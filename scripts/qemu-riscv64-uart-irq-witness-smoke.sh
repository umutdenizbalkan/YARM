#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-IRQ1 — the first real external-interrupt witness: RISC-V UART0 -> PLIC -> YARM.
#
# Usage: scripts/qemu-riscv64-uart-irq-witness-smoke.sh
#   LOGDIR=...        where build/boot logs land (default /tmp/qemu-riscv64-uart-irq-witness)
#   SKIP_BUILD=1      reuse the artifacts already in $LOGDIR/build
#   DRIVER_ARGS=...   extra driver flags (e.g. --no-inject for the withheld-producer control)
#   REGRADE=1         grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#
# One boot of QEMU virt (-smp 1, OpenSBI) built with `riscv-uart-irq-witness`. The host driver
# (scripts/qemu-riscv64-uart-irq-driver.py) owns UART0's only serial backend — a UNIX socket, no
# monitor, no multiplexing — and injects one byte per item only after the guest's READY line
# (and, for idle items, after the kernel reports the hart idle). This script grades what the
# boot log shows; it never retries.
#
# Graded, each separately:
#   * RECEIVER: init's own summary — 8 items received exactly once through NR 5 on the bound
#     notification, labels = the DTB line, data sequence exact, 4 idle + 4 user items, registers
#     intact, source off afterwards with 10 timed-out parks and no notification/byte/claim, and
#     both isolation probes refused.
#   * CONTROLLER: the window is installed and kernel-only, readiness is reachable from the walk,
#     the source is enabled with the DTB id and S-context.
#   * ACCOUNTING: claims == completions == drains-with-one-byte == deliveries == probes == 8,
#     no NoPending/Unavailable claim, no foreign source, every drain leaves LSR.DR clear.
#   * ORIGINS: 4 idle entries whose sepc lies in the kernel's idle wait, 4 user entries of which
#     at least one lies inside the receiver's register-checked spin.
#   * ORDER: every injection precedes the interrupt entry it produced; nothing is claimed after
#     the source is disabled, while ticks and the supervisor loop continue.
#   * ISOLATION: the child's load from the claim register's window VA took an unhandled page
#     fault at exactly that address.
#   * NOTHING FATAL: no unhandled S-mode trap, no handler failure, no panic, no unexpected resume.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-riscv64-uart-irq-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
KELF_COPY="$LOGDIR/kernel_boot.elf"
UELF_COPY="$LOGDIR/init_server.elf"
fail=0
note() { echo "[uart-irq-witness] $*"; }
die()  { echo "[uart-irq-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  note "building riscv64 artifacts with riscv-uart-irq-witness"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features riscv-uart-irq-witness" \
    scripts/build-qemu-riscv64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "UART_IRQ_WITNESS_SEAL result=fail reason=build"; exit 1; }
  cp target/riscv64gc-unknown-none-elf/release/kernel_boot "$KELF_COPY"
  cp target/riscv64-yarm-user-none/release/init_server "$UELF_COPY"
fi

if [[ "${REGRADE:-0}" == "1" ]]; then
  # Grade an existing boot log again (a grading-script fix), never a second boot.
  note "re-grading $BOOT_LOG without booting"
  driver_status=$(grep -a -o '#HOST_SUMMARY .* status=[0-9]*' "$BOOT_LOG" | sed 's/.*status=//' | tail -1)
  driver_status=${driver_status:-9}
else
  note "booting (dedicated socket serial, host injection on READY)"
  # shellcheck disable=SC2086
  python3 scripts/qemu-riscv64-uart-irq-driver.py \
    --kernel "$BUILD_DIR/yarm-riscv64.bin" --initrd "$BUILD_DIR/initramfs-core.cpio" \
    --log "$BOOT_LOG" --timeout "${TIMEOUT_SECS:-240}" ${DRIVER_ARGS:-}
  driver_status=$?
fi
tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
[[ $driver_status -eq 0 ]] || die "driver status $driver_status (0 = witness summary seen)"

count() { grep -a -c -F -- "$1" "$NORM" 2>/dev/null || true; }
line_of() { grep -a -m1 -n -F -- "$1" "$NORM" | cut -d: -f1; }
has_all() { # line props...
  local l="$1"; shift
  for p in "$@"; do [[ " $l " == *" $p "* ]] || { die "missing '$p' in: ${l:0:220}"; }; done
}

# ── RECEIVER ────────────────────────────────────────────────────────────────────────────────
n=$(count 'IRQ1_UART_WITNESS items=')
[[ "$n" == "1" ]] || die "expected exactly one receiver summary (got $n)"
summary=$(grep -a -m1 'IRQ1_UART_WITNESS items=' "$NORM" | sed 's/.*msg=//')
has_all "$summary" items=8 received=8 exact_once=8 label_ok=8 data_ok=8 idle_items=4 user_items=4 \
  regs_bad=0 dup=0 errors=0 isolated=1 result=ok
counts=$(grep -a -m1 'IRQ1_UART_WITNESS_COUNTS' "$NORM" | sed 's/.*msg=//')
has_all "$counts" claims=8 completions=8 empty_drains=0 idle_origin=4 user_origin=4 disabled=1 \
  post_rounds=10 post_timeouts=10 post_notifications=0 post_quiet=1
for s in 1 2 3 4 5 6 7 8; do
  b=$(printf '0x%02x' $((0x40 + s)))
  [[ $(grep -a -c "IRQ1_UART_RECV seq=$s mode=[a-z]* label=10 payload_len=2 once=1 byte=$b bytes=$s data_ok=1 " "$NORM") == 1 ]] \
    || die "item $s not received exactly once with byte $b"
done

# ── CONTROLLER ──────────────────────────────────────────────────────────────────────────────
[[ $(count 'IRQ1_DEVICE_WINDOW_INSTALLED va=0x3fc0000000 slot=255 pages=4') == 1 ]] || die "window not installed"
[[ $(count 'IRQ1_DEVICE_WINDOW_ISOLATION user_va_admissible=0 leaf_user=0 leaf_exec=0 dtb_uart=0x10000000 dtb_uart_irq=10') == 1 ]] \
  || die "window isolation facts missing"
[[ $(count 'RISCV_EXTIRQ_CLAIM_READINESS context=1 addr=0xc201004 configured=1 reachable=1 reason=claimable') == 1 ]] \
  || die "claim readiness not derived reachable"
[[ $(count 'RISCV_EXTIRQ_SMOKE_OK source=10 context=1 priority=1 threshold=0 enable_word=0x400 ier=0x1 seie=1') == 1 ]] \
  || die "source not enabled as derived"
[[ $(count 'IRQ1_WITNESS_PROVISION_OK init_tid=1 irq_line=10 ') == 1 ]] || die "route not provisioned"

# ── ACCOUNTING ──────────────────────────────────────────────────────────────────────────────
claims=$(count 'IRQFINAL_RISCV_CLAIMED cpu=0 source=10 context=1 ')
reads=$(count 'RISCV_EXTIRQ_CLAIM_READ ')
completes=$(count 'IRQ1_UART_COMPLETE n=')
drains1=$(grep -a -c 'IRQ1_UART_DRAIN claim=[0-9]* source=10 context=1 bytes=1 .*lsr_dr_after=0 ' "$NORM")
drains=$(count 'IRQ1_UART_DRAIN claim=')
delivered=$(count 'IRQ1_SPLIT_DELIVERY cpu=0 line=10 outcome=delivered')
probes=$(count 'IRQ1_NOTIFICATION_PROBE_DELIVERED tid=1 notification=0 generation=1 label=10')
nopend=$(count 'IRQFINAL_RISCV_NO_PENDING_CLAIM')
unavail=$(count 'IRQFINAL_RISCV_CLAIM_UNAVAILABLE')
foreign=$(count 'IRQ1_UART_FOREIGN_CLAIM')
for v in claims reads completes drains1 drains delivered probes; do
  [[ "${!v}" == "8" ]] || die "$v=${!v}, expected 8"
done
for v in nopend unavail foreign; do
  [[ "${!v}" == "0" ]] || die "$v=${!v}, expected 0"
done

# ── ORIGINS ─────────────────────────────────────────────────────────────────────────────────
idle_n=$(count 'IRQ1_UART_IRQ_ENTRY origin=idle')
user_n=$(count 'IRQ1_UART_IRQ_ENTRY origin=user')
[[ "$idle_n" == "4" && "$user_n" == "4" ]] || die "origins idle=$idle_n user=$user_n, expected 4/4"
sym_range() { # elf symbol-substring -> "start end" (decimal)
  local addr size
  read -r addr size _ < <(nm -S -C "$1" 2>/dev/null | grep -F -- "$2" | head -1)
  [[ -n "${addr:-}" && -n "${size:-}" ]] || { echo "0 0"; return; }
  echo "$((16#$addr)) $((16#$addr + 16#$size))"
}
in_idle_wait=0; in_spin=0
if [[ -f "$KELF_COPY" ]]; then
  read -r ws we < <(sym_range "$KELF_COPY" "timer::halt_wait_loop")
  while read -r pc; do
    (( pc >= ws && pc < we )) && in_idle_wait=$((in_idle_wait + 1))
  done < <(grep -a 'IRQ1_UART_IRQ_ENTRY origin=idle' "$NORM" | sed 's/.*sepc=0x\([0-9a-f]*\).*/\1/' | while read -r h; do echo $((16#$h)); done)
  [[ "$in_idle_wait" == "$idle_n" ]] || die "idle-origin sepc outside the idle wait ($in_idle_wait/$idle_n)"
else
  die "kernel ELF copy missing; cannot place idle sepc"
fi
if [[ -f "$UELF_COPY" ]]; then
  # Either register-checked block: the READY announce+spin, or a later spin round.
  read -r as ae < <(sym_range "$UELF_COPY" "uart_irq_witness::announce_ready_and_spin")
  read -r ss se < <(sym_range "$UELF_COPY" "uart_irq_witness::spin_checking_registers")
  while read -r pc; do
    if (( (pc >= as && pc < ae) || (pc >= ss && pc < se) )); then in_spin=$((in_spin + 1)); fi
  done < <(grep -a 'IRQ1_UART_IRQ_ENTRY origin=user' "$NORM" | sed 's/.*sepc=0x\([0-9a-f]*\).*/\1/' | while read -r h; do echo $((16#$h)); done)
  (( in_spin >= 1 )) || die "no user-origin interrupt landed inside the register-checked spin"
fi

# ── ORDER ───────────────────────────────────────────────────────────────────────────────────
injects=$(grep -a -c '^#HOST_INJECT byte=0x4[1-8] .* seq=' "$NORM")
[[ "$injects" == "8" ]] || die "expected 8 item injections (got $injects)"
for s in 1 2 3 4 5 6 7 8; do
  inj=$(grep -a -n "^#HOST_INJECT .* seq=$s " "$NORM" | head -1 | cut -d: -f1)
  entry=$(grep -a -n 'IRQ1_UART_IRQ_ENTRY ' "$NORM" | sed -n "${s}p" | cut -d: -f1)
  [[ -n "$inj" && -n "$entry" ]] && (( inj < entry )) || die "item $s: injection does not precede its interrupt"
done
dis=$(line_of 'IRQ1_UART_SOURCE_DISABLED source=10 claims=8 data_claims=8 completions=8 bytes=8 enable_word_after=0x0 ier=0')
if [[ -z "$dis" ]]; then
  die "source was not disabled after the last item"
else
  post_entries=$(tail -n +"$dis" "$NORM" | grep -a -c 'IRQ1_UART_IRQ_ENTRY\|RISCV_EXTIRQ_CLAIM_READ ')
  post_inject=$(tail -n +"$dis" "$NORM" | grep -a -c '^#HOST_INJECT byte=0x5a .*post_disable=1')
  post_ticks=$(tail -n +"$dis" "$NORM" | grep -a -c 'TIMER_SPLIT_TICK_OK')
  post_service=$(tail -n +"$dis" "$NORM" | grep -a -c 'SUPERVISOR_EVENT_LOOP_TICK')
  [[ "$post_entries" == "0" ]] || die "$post_entries interrupt entries after the source was disabled"
  [[ "$post_inject" == "1" ]] || die "the post-disable byte was not injected"
  (( post_ticks > 0 )) || die "no timer progress after the disable"
  (( post_service > 0 )) || die "no service progress after the disable"
fi

# ── ISOLATION ───────────────────────────────────────────────────────────────────────────────
[[ $(grep -a -c 'PAGE_FAULT_UNHANDLED tid=[0-9]* addr=0x3fc0002004 access=Read' "$NORM") == 1 ]] \
  || die "the user load from the window did not take an unhandled fault at the claim VA"
iso=$(grep -a -m1 'IRQ1_UART_ISOLATION' "$NORM" | sed 's/.*msg=//')
has_all "$iso" load_returned=0 anon_map_over_window_refused=1 result=ok

# ── NOTHING FATAL ───────────────────────────────────────────────────────────────────────────
for bad in 'RISCV_TRAP_UNHANDLED' 'RISCV_TRAP_HANDLE_FAILED' 'panicked at' 'KERNEL PANIC' \
           'RISCV_S_MODE_EXTIRQ_UNEXPECTED_RESUME' 'IRQFINAL_RISCV_COMPLETION_UNTRANSLATABLE' \
           'IRQ1_DEVICE_WINDOW_REFUSED' '#HOST_ERROR'; do
  n=$(count "$bad"); [[ "$n" == "0" ]] || die "$bad appeared $n times"
done
halts=$(grep -a 'RISCV_TRAP_HALTED reason=' "$NORM" | grep -a -v -c 'reason=kernel_idle_awaiting_io')
[[ "$halts" == "0" ]] || die "$halts unexpected halt reasons"

if (( fail )); then
  echo "UART_IRQ_WITNESS_SEAL result=fail"
  exit 1
fi
echo "UART_IRQ_WITNESS_SEAL items=8 claims=$claims completions=$completes deliveries=$delivered probes=$probes idle=$idle_n user=$user_n idle_in_wait=$in_idle_wait user_in_spin=$in_spin post_ticks=$post_ticks post_service=$post_service result=ok"
