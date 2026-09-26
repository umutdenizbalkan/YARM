#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-IRQ2 — the AArch64 external-device interrupt witness: PL011 RX -> GICv2 SPI 1 (INTID 33)
# -> YARM, on QEMU virt / cortex-a72 / 1024M / -smp 1 (the core smoke's machine at one CPU).
#
# Usage: scripts/qemu-aarch64-pl011-irq-witness-smoke.sh
#   LOGDIR=...        where build/boot logs land (default /tmp/qemu-aarch64-pl011-irq-witness)
#   SKIP_BUILD=1      reuse the artifacts already in $LOGDIR/build
#   DRIVER_ARGS=...   extra driver flags (e.g. --no-inject for the withheld-producer control)
#   REGRADE=1         grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#
# One boot built with `aarch64-pl011-irq-witness`. The host driver
# (scripts/qemu-riscv64-uart-irq-driver.py --arch aarch64) owns the PL011's only serial backend —
# a UNIX socket, no monitor, no multiplexing — and injects one byte per item only after the
# guest's READY line (and, for idle items, after a timer tick has settled back to the idle loop with
# nothing runnable).
# This script grades what the boot log shows; it never retries.
#
# Graded, each separately:
#   * RECEIVER: init's own summary — 8 items received exactly once through NR 5 on the bound
#     notification, labels = INTID 33, data sequence exact, 4 idle + 4 user items, GPR and SIMD
#     sentinels intact across EL0-origin interrupts, source off afterwards with 10 timed-out parks
#     and no notification/byte/claim, and both isolation probes refused.
#   * CONTROLLER: the DTB's `<0 1 4>` read as INTID 33 level-high; the PL011, GICD and GICC pages
#     resolve under the live TTBR0 as EL1 Device-nGnRE, are denied to EL0 and are execute-never at
#     both levels; the SPI configured group 0 / priority 0x80 / level, IMSC=RXIM, ISENABLER set.
#   * ACCOUNTING: per claim, in order, entry -> one-byte drain leaving the FIFO empty and MIS clear
#     -> production delivery -> completion writing the IAR token 0x21 with the distributor then
#     reporting the SPI neither active nor pending and the running priority idle. 8 of each.
#   * ORIGINS: 4 idle entries (irq_current_spx, parked) whose ELR is the take point of the sized
#     idle-window leaf (`wfi; daifclr; daifset; ret` -> the `daifset`), 4 user entries (irq_lower_a64) of which at least one lies inside the
#     receiver's register-checked spin.
#   * ORDER: each injection precedes its entry; nothing is claimed after the disable, while the
#     timer, the receiver's timed parks and init's own progress continue.
#   * ISOLATION: the child's EL0 load from the PL011 flag register took an unhandled fault at that
#     address, and the anonymous mapping over the PL011 page was refused.
#   * NOTHING FATAL.
# Reported, not graded: the idle-resume SIMD loss (see doc/KERNEL_UNLOCKING.md, QEMU-IRQ2).
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-pl011-irq-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
KELF_COPY="$LOGDIR/kernel_boot.elf"
UELF_COPY="$LOGDIR/init_server.elf"
fail=0
note() { echo "[pl011-irq-witness] $*"; }
die()  { echo "[pl011-irq-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  note "building aarch64 artifacts with aarch64-pl011-irq-witness"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features aarch64-pl011-irq-witness" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "PL011_IRQ_WITNESS_SEAL result=fail reason=build"; exit 1; }
  cp "$(ls target/aarch64-yarm-none/*/kernel_boot | head -1)" "$KELF_COPY"
  cp "$(ls target/aarch64-yarm-user-none/*/init_server | head -1)" "$UELF_COPY"
fi

if [[ "${REGRADE:-0}" == "1" ]]; then
  note "re-grading $BOOT_LOG without booting"
  driver_status=$(grep -a -o '#HOST_SUMMARY .* status=[0-9]*' "$BOOT_LOG" | sed 's/.*status=//' | tail -1)
  driver_status=${driver_status:-9}
else
  note "booting (dedicated socket serial, host injection on READY)"
  # shellcheck disable=SC2086
  python3 scripts/qemu-riscv64-uart-irq-driver.py --arch aarch64 \
    --kernel "$BUILD_DIR/yarm-aarch64.bin" --initrd "$BUILD_DIR/initramfs-core.cpio" \
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
  [[ $(grep -a -c "IRQ1_UART_RECV seq=$s mode=[a-z]* label=33 payload_len=2 once=1 byte=$b bytes=$s data_ok=1 rounds=[0-9]* regs_mask=0x0 " "$NORM") == 1 ]] \
    || die "item $s not received exactly once with byte $b and intact sentinels"
done

# ── CONTROLLER ──────────────────────────────────────────────────────────────────────────────
[[ $(count 'IRQ2_PL011_DTB base=0x9000000 spi=1 intid=33 trigger=4 level=1') == 1 ]] || die "DTB not read as SPI 1 -> INTID 33 level"
for page in uart gicd gicc; do
  [[ $(grep -a -c "IRQ2_PL011_MMIO_ACCESS page=$page pa=0x[0-9a-f]* asid=[0-9]* el1_device=1 el0_denied=1 par_el1=0x4[0-9a-f]* level=[123] desc=0x[0-9a-f]* ap_el0=0 uxn=[01] pxn=[01] attr_idx=3" "$NORM") == 1 ]] \
    || die "$page page not proven EL1-Device / EL0-denied under the enable-time TTBR0"
done
# Every claim re-derives the regime it was taken under: all three pages kernel-only Device-nGnRE
# and execute-never at both levels, the PL011 through a 4 KiB level-3 leaf of that root.
regime=$(grep -a -c 'IRQ2_PL011_IRQ_ENTRY .* regime_ok=1 uart_level=3 ' "$NORM")
[[ "$regime" == "8" ]] || die "only $regime/8 claims ran under a proven kernel-only execute-never regime"
enabled=$(grep -a -m1 'IRQ2_PL011_SOURCE_ENABLED' "$NORM" | sed 's/.*IRQ2_PL011_SOURCE_ENABLED/IRQ2_PL011_SOURCE_ENABLED/')
has_all "$enabled" intid=33 spi=1 group=0 priority=0x80 trigger=level imsc=0x10 isenabler=1 items=8
[[ $(count 'IRQ1_WITNESS_PROVISION_OK init_tid=1 irq_line=33 ') == 1 ]] || die "route not bound to INTID 33"
[[ $(count 'IRQ2_WITNESS_SLOTS slot5=30 ') == 1 ]] || die "receiver slots not provisioned"
prov=$(line_of 'IRQ1_WITNESS_PROVISION_OK'); en=$(line_of 'IRQ2_PL011_SOURCE_ENABLED'); unm=$(line_of 'AARCH64_BSP_TIMER_STARTED')
[[ -n "$prov" && -n "$en" && -n "$unm" ]] && (( prov < en && en < unm )) \
  || die "order is not route bound -> source enabled -> PE unmask ($prov/$en/$unm)"

# ── ACCOUNTING ──────────────────────────────────────────────────────────────────────────────
entries=$(count 'IRQ2_PL011_IRQ_ENTRY ')
drains1=$(grep -a -c 'IRQ2_PL011_DRAIN claim=[0-9]* intid=33 bytes=1 .*mis_before=0x10 rxfe_after=1 .*mis_after=0x0 ' "$NORM")
delivered=$(count 'IRQ1_SPLIT_DELIVERY cpu=0 line=33 outcome=delivered')
probes=$(count 'IRQ1_NOTIFICATION_PROBE_DELIVERED tid=1 notification=0 generation=1 label=33')
completes=$(grep -a -c 'IRQ2_PL011_COMPLETE n=[0-9]* token=0x21 intid=33 claims=[0-9]* active_after=0 pending_after=0 rpr_after=0xff' "$NORM")
for v in entries drains1 delivered probes completes; do
  [[ "${!v}" == "8" ]] || die "$v=${!v}, expected 8"
done
for bad in 'IRQ2_PL011_FOREIGN_CLAIM' 'reason=uart_unreachable'; do
  n=$(count "$bad"); [[ "$n" == "0" ]] || die "$bad appeared $n times"
done
# Per claim, in order: entry -> drain -> delivery -> completion, and never a second of any.
seq_ok=$(grep -a -o 'IRQ2_PL011_IRQ_ENTRY\|IRQ2_PL011_DRAIN\|IRQ1_SPLIT_DELIVERY cpu=0 line=33\|IRQ2_PL011_COMPLETE' "$NORM" \
  | awk 'BEGIN{e="IRQ2_PL011_IRQ_ENTRY";d="IRQ2_PL011_DRAIN";v="IRQ1_SPLIT_DELIVERY cpu=0 line=33";c="IRQ2_PL011_COMPLETE";want=e;n=0;ok=1}
         {if($0!=want){ok=0;exit} if(want==e)want=d; else if(want==d)want=v; else if(want==v)want=c; else {want=e;n++}}
         END{print (ok && want==e) ? n : -1}')
[[ "$seq_ok" == "8" ]] || die "claim/drain/delivery/completion are not 8 ordered quadruples ($seq_ok)"
specials=$(count 'IRQ2_GIC_SPECIAL_CLAIM ')
special_completed=$(grep -a -c 'IRQ2_GIC_SPECIAL_CLAIM .* completed=1' "$NORM")
[[ "$special_completed" == "0" ]] || die "a special INTID was completed"

# ── ORIGINS ─────────────────────────────────────────────────────────────────────────────────
idle_n=$(grep -a -c 'IRQ2_PL011_IRQ_ENTRY origin=idle n=[0-9]* claim=[0-9]* intid=33 kind=6 spsr=0x[0-9a-f]*5 elr=0x[0-9a-f]* parked=1 ' "$NORM")
user_n=$(grep -a -c 'IRQ2_PL011_IRQ_ENTRY origin=user n=[0-9]* claim=[0-9]* intid=33 kind=10 spsr=0x[0-9a-f]*0 elr=0x[0-9a-f]* parked=0 tid=1' "$NORM")
other_n=$(count 'IRQ2_PL011_IRQ_ENTRY origin=other')
[[ "$idle_n" == "4" && "$user_n" == "4" && "$other_n" == "0" ]] || die "origins idle=$idle_n user=$user_n other=$other_n, expected 4/4/0"
sym_range() { # elf symbol-substring -> "start end" (decimal)
  local addr size
  read -r addr size _ < <(nm -S -C "$1" 2>/dev/null | grep -F -- "$2" | head -1)
  [[ -n "${addr:-}" && -n "${size:-}" ]] || { echo "0 0"; return; }
  echo "$((16#$addr)) $((16#$addr + 16#$size))"
}
elrs() { grep -a "IRQ2_PL011_IRQ_ENTRY origin=$1" "$NORM" | sed 's/.* elr=0x\([0-9a-f]*\).*/\1/' | while read -r h; do echo $((16#$h)); done; }
in_window=0; in_spin=0
if [[ -f "$KELF_COPY" ]]; then
  read -r ws we < <(sym_range "$KELF_COPY" "yarm_aarch64_idle_wfi_window")
  (( we - ws == 16 )) || die "the unmasked window is not the 16-byte leaf ($ws..$we)"
  # `wfi; daifclr; daifset; ret`: an interrupt is taken only after the `daifclr`, so its ELR is
  # exactly the `daifset` at +8.
  while read -r pc; do (( pc == ws + 8 )) && in_window=$((in_window + 1)); done < <(elrs idle)
  [[ "$in_window" == "$idle_n" ]] || die "idle-origin ELR not at the window's take point ($in_window/$idle_n)"
else
  die "kernel ELF copy missing; cannot place idle ELR"
fi
if [[ -f "$UELF_COPY" ]]; then
  read -r as ae < <(sym_range "$UELF_COPY" "uart_irq_witness::announce_ready_and_spin")
  read -r ss se < <(sym_range "$UELF_COPY" "uart_irq_witness::spin_checking_registers")
  while read -r pc; do
    if (( (pc >= as && pc < ae) || (pc >= ss && pc < se) )); then in_spin=$((in_spin + 1)); fi
  done < <(elrs user)
  (( in_spin >= 1 )) || die "no user-origin interrupt landed inside the register-checked spin"
fi

# ── ORDER ───────────────────────────────────────────────────────────────────────────────────
injects=$(grep -a -c '^#HOST_INJECT byte=0x4[1-8] .* seq=' "$NORM")
[[ "$injects" == "8" ]] || die "expected 8 item injections (got $injects)"
for s in 1 2 3 4 5 6 7 8; do
  inj=$(grep -a -n "^#HOST_INJECT .* seq=$s " "$NORM" | head -1 | cut -d: -f1)
  entry=$(grep -a -n 'IRQ2_PL011_IRQ_ENTRY ' "$NORM" | sed -n "${s}p" | cut -d: -f1)
  [[ -n "$inj" && -n "$entry" ]] && (( inj < entry )) || die "item $s: injection does not precede its interrupt"
done
dis=$(line_of 'IRQ2_PL011_SOURCE_DISABLED intid=33 claims=8 data_claims=8 completions=8 deactivated=8 bytes=8 imsc_after=0x0 isenabler_after=0')
post_ticks=0; post_parks=0
if [[ -z "$dis" ]]; then
  die "source was not disabled after the last item, with every completion deactivated"
else
  post_entries=$(tail -n +"$dis" "$NORM" | grep -a -c 'IRQ2_PL011_IRQ_ENTRY\|IRQ2_PL011_FOREIGN_CLAIM\|IRQ1_SPLIT_DELIVERY')
  post_inject=$(tail -n +"$dis" "$NORM" | grep -a -c '^#HOST_INJECT byte=0x5a .*post_disable=1')
  post_ticks=$(tail -n +"$dis" "$NORM" | grep -a -c 'TIMER_SPLIT_IDLE_ADVANCE_COMMITTED\|TIMER_SPLIT_TICK_OK')
  post_parks=$(tail -n +"$dis" "$NORM" | grep -a -c 'U8_RECV_TIMEOUT_SETTLED arch=aarch64 tid=1 ')
  post_init=$(tail -n +"$dis" "$NORM" | grep -a -c 'INIT_IDLE_PARK_BEGIN')
  [[ "$post_entries" == "0" ]] || die "$post_entries interrupt entries/deliveries after the source was disabled"
  [[ "$post_inject" == "1" ]] || die "the post-disable byte was not injected"
  (( post_ticks > 0 )) || die "no timer progress after the disable"
  (( post_parks >= 10 )) || die "the receiver's timed parks did not keep expiring ($post_parks)"
  (( post_init >= 1 )) || die "init did not progress past the witness"
fi

# ── ISOLATION ───────────────────────────────────────────────────────────────────────────────
[[ $(grep -a -c 'PAGE_FAULT_UNHANDLED tid=[0-9]* addr=0x9000018 access=Read' "$NORM") == 1 ]] \
  || die "the EL0 load from the PL011 did not take an unhandled fault at that address"
iso=$(grep -a -m1 'IRQ1_UART_ISOLATION' "$NORM" | sed 's/.*msg=//')
has_all "$iso" window_va=0x9000018 load_returned=0 anon_map_over_window_refused=1 result=ok

# ── NOTHING FATAL ───────────────────────────────────────────────────────────────────────────
for bad in 'panicked at' 'KERNEL PANIC' 'YARM_AARCH64_TRAP_HANDLE failed' 'YARM_AARCH64_TRAP_HANDLE no_kernel_state' \
           'IRQ2_PL011_ENABLE_DEFERRED' 'AARCH64_DIRECT_DISPATCH_FATAL' 'AARCH64_IDLE_BOUNDARY_RETURN_REFUSED' \
           '#HOST_ERROR'; do
  n=$(count "$bad"); [[ "$n" == "0" ]] || die "$bad appeared $n times"
done

# ── REPORTED, NOT GRADED ────────────────────────────────────────────────────────────────────
simd_loss=$(grep -a 'IRQ1_UART_RECV seq=[0-9]* mode=idle' "$NORM" | grep -a -v -c 'idle_resume_simd_mask=0x0')
note "idle-resume SIMD sentinel loss in $simd_loss/4 idle items (pre-existing: no per-task FP/SIMD state)"
note "special INTID claims: $specials (never completed)"

if (( fail )); then
  echo "PL011_IRQ_WITNESS_SEAL result=fail"
  exit 1
fi
echo "PL011_IRQ_WITNESS_SEAL items=8 intid=33 claims=$entries completions=$completes deliveries=$delivered probes=$probes idle=$idle_n user=$user_n idle_in_window=$in_window user_in_spin=$in_spin post_ticks=$post_ticks post_parks=$post_parks specials=$specials result=ok"
