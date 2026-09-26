#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-IRQ3 — the x86_64 external-device interrupt witness: COM1 (16550, I/O 0x3F8) RX -> ISA IRQ 4
# -> I/O APIC pin (GSI from the live MADT) -> LAPIC vector 0x20+GSI -> YARM, on the x86_64 core
# smoke's machine: q35 / qemu64 / 512M / -smp 1, PVH direct boot.
#
# Usage: scripts/qemu-x86_64-uart-irq-witness-smoke.sh
#   LOGDIR=...        where build/boot logs land (default /tmp/qemu-x86_64-uart-irq-witness)
#   SKIP_BUILD=1      reuse the artifacts already in $LOGDIR/build
#   DRIVER_ARGS=...   extra driver flags (e.g. --no-inject for the withheld-producer control)
#   REGRADE=1         grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#
# One boot built with `x86_64-uart-irq-witness`. The host driver
# (scripts/qemu-riscv64-uart-irq-driver.py --arch x86_64) owns COM1's only serial backend — a UNIX
# socket, no monitor, no multiplexing — and injects one byte per item only after the guest's READY
# line (and, for idle items, after a timer tick has settled back to the idle loop with nothing
# runnable). This script grades what the boot log shows; it never retries.
#
# Graded, each separately:
#   * RECEIVER: init's own summary — 8 items received exactly once through NR 5 on the bound
#     notification, labels = the derived line, data sequence exact, 4 idle + 4 user items, GPR and
#     carry-flag sentinels intact across ring-3-origin interrupts, source off afterwards with 10
#     timed-out parks and no notification/byte/claim, and both isolation probes refused.
#   * ROUTE: the MADT read through the PVH RSDP; ISA IRQ 4 -> GSI 4 (no override), active-high edge,
#     I/O APIC 0 at 0xFEC00000 base 0, vector 0x24 = 0x20 + line 4; the 8259 input masked; the
#     redirection entry fixed/physical to the BSP's APIC ID; the route bound before the enable, and
#     the enable done with interrupts still off.
#   * ACCESS: the I/O APIC and LAPIC pages present, uncached and supervisor-only in the live CR3 at
#     the enable and at every claim; the TSS carries no I/O bitmap (IOPL 0 => every ring-3 port
#     access faults).
#   * ACCOUNTING: per claim, in order, entry -> one-byte drain leaving IIR "no interrupt" and no
#     re-delivery of the vector queued -> production delivery -> completion with exactly ONE EOI by
#     the production owner, the LAPIC in-service bit then clear, Remote-IRR and delivery-pending
#     clear. 8 of each, 0 empty drains.
#   * TOGETHER: on two items (one idle, one user) the timer vector was pending in the LAPIC IRR
#     while the device vector was in service, and both then completed.
#   * ORIGINS: 4 idle entries (CS 0x08, parked, tid 0) whose RIP is the instruction after the idle
#     loop's `sti; hlt`; 4 ring-3 entries (CS 0x23, tid 1) of which at least one lies inside the
#     receiver's register-checked spin; every idle claim re-parks through the idle path.
#   * ORDER: each injection precedes its entry; nothing is claimed after the disable, while the
#     timer, the receiver's timed parks and init's own progress continue.
#   * ISOLATION: the child's ring-3 load from the I/O APIC window took an unhandled fault at that
#     address, and the anonymous mapping over it was refused.
#   * NOTHING FATAL.
# Reported, not graded: the XMM sentinels (x86_64 does not preserve user XMM state across kernel
# entry; see doc/KERNEL_UNLOCKING.md, QEMU-IRQ3).
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-x86_64-uart-irq-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
NORM="$LOGDIR/boot.norm.log"
KELF_COPY="$LOGDIR/kernel_boot.elf"
UELF_COPY="$LOGDIR/init_server.elf"
fail=0
note() { echo "[x86-uart-irq-witness] $*"; }
die()  { echo "[x86-uart-irq-witness][fail] $*"; fail=1; }

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  note "building x86_64 artifacts with x86_64-uart-irq-witness"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features x86_64-uart-irq-witness" \
    scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "X86_UART_IRQ_WITNESS_SEAL result=fail reason=build"; exit 1; }
  cp "$BUILD_DIR/kernel_boot.debug.elf" "$KELF_COPY"
  cp "$(ls target/x86_64-yarm-user-none/*/init_server | head -1)" "$UELF_COPY"
fi

if [[ "${REGRADE:-0}" == "1" ]]; then
  note "re-grading $BOOT_LOG without booting"
  driver_status=$(grep -a -o '#HOST_SUMMARY .* status=[0-9]*' "$BOOT_LOG" | sed 's/.*status=//' | tail -1)
  driver_status=${driver_status:-9}
else
  note "booting (dedicated socket serial, host injection on READY)"
  # shellcheck disable=SC2086
  python3 scripts/qemu-riscv64-uart-irq-driver.py --arch x86_64 \
    --kernel "$BUILD_DIR/kernel_boot.elf" --initrd "$BUILD_DIR/initramfs-core.cpio" \
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
[[ $(count 'IRQ1_UART_ARMED rounds=0 expect_label=4') == 1 ]] || die "receiver did not read bound line 4 from the ring"
for s in 1 2 3 4 5 6 7 8; do
  b=$(printf '0x%02x' $((0x40 + s)))
  [[ $(grep -a -c "IRQ1_UART_RECV seq=$s mode=[a-z]* label=4 payload_len=2 once=1 byte=$b bytes=$s data_ok=1 rounds=[0-9]* regs_mask=0x0 " "$NORM") == 1 ]] \
    || die "item $s not received exactly once with byte $b and intact GPR/flag sentinels"
done

# ── ROUTE ───────────────────────────────────────────────────────────────────────────────────
[[ $(grep -a -c 'IRQ3_UART_MADT len=[0-9]* ioapic_id=0 ioapic_pa=0xfec00000 gsi_base=0 overrides=[0-9]* ' "$NORM") == 1 ]] \
  || die "MADT not read (I/O APIC 0 at 0xfec00000, base 0)"
[[ $(count 'IRQ3_UART_ROUTE isa_irq=4 gsi=4 overridden=0 active_low=0 level=0 ioapic_pin=4 vector=0x24 line=4') == 1 ]] \
  || die "route not derived as ISA 4 -> GSI 4 edge/high -> pin 4 -> vector 0x24 -> line 4"
[[ $(count 'IRQ3_UART_PORT_ACCESS port=0x3f8 tss_io_map_base=104 tss_size=104 io_bitmap=false pic_master_imr=0xff pic_slave_imr=0xff pic_irq_masked=1') == 1 ]] \
  || die "port access / competing 8259 route not as derived"
dev=$(grep -a -m1 'IRQ3_UART_DEVICE_ENABLED' "$NORM" | sed 's/.*IRQ3_UART_DEVICE_ENABLED/IRQ3_UART_DEVICE_ENABLED/')
has_all "$dev" port=0x3f8 cpu_if=0 ier=0x1 stale_drained=0
[[ $(count 'IRQ3_UART_CPU_ADMISSION vector=0x24 if_restored=1 after=source_enabled') == 1 ]] \
  || die "CPU admission not restored after the source was enabled"
enabled=$(grep -a -m1 'IRQ3_UART_SOURCE_ENABLED' "$NORM" | sed 's/.*IRQ3_UART_SOURCE_ENABLED/IRQ3_UART_SOURCE_ENABLED/')
has_all "$enabled" gsi=4 ioapic_pin=4 vector=0x24 line=4 dest_apic=0 redir_low=0x24 redir_high=0x0 ioapic_pins=24 items=8
[[ $(count 'IRQ1_WITNESS_PROVISION_OK init_tid=1 irq_line=4 ') == 1 ]] || die "route not bound to line 4"
[[ $(count 'IRQ3_WITNESS_SLOTS slot5=30 ') == 1 ]] || die "receiver slots not provisioned"
prov=$(line_of 'IRQ1_WITNESS_PROVISION_OK'); tim=$(line_of 'X86_BOOTSTRAP_TIMER_STARTED'); acc=$(line_of 'IRQ3_UART_MMIO_ACCESS page=ioapic')
dv=$(line_of 'IRQ3_UART_DEVICE_ENABLED'); en=$(line_of 'IRQ3_UART_SOURCE_ENABLED'); adm=$(line_of 'IRQ3_UART_CPU_ADMISSION')
[[ -n "$prov" && -n "$tim" && -n "$acc" && -n "$dv" && -n "$en" && -n "$adm" ]] && (( prov < tim && tim < acc && acc < dv && dv < en && en < adm )) \
  || die "order is not route bound -> timer armed -> access -> device -> source -> CPU admission ($prov/$tim/$acc/$dv/$en/$adm)"

# ── ACCESS ──────────────────────────────────────────────────────────────────────────────────
for page in ioapic lapic; do
  [[ $(grep -a -c "IRQ3_UART_MMIO_ACCESS page=$page va=0x[0-9a-f]* present=1 uncached=1 user_reachable=0 " "$NORM") == 1 ]] \
    || die "$page page not proven present / uncached / supervisor-only in the live CR3"
done
regime=$(grep -a -c 'IRQ3_UART_IRQ_ENTRY .* regime_ok=1' "$NORM")
[[ "$regime" == "8" ]] || die "only $regime/8 claims ran under a proven supervisor-only uncached regime"

# ── ACCOUNTING ──────────────────────────────────────────────────────────────────────────────
entries=$(count 'IRQ3_UART_IRQ_ENTRY ')
in_service=$(grep -a -c 'IRQ3_UART_IRQ_ENTRY .* vector=0x24 line=4 .* lapic_isr=1 ' "$NORM")
drains1=$(grep -a -c 'IRQ3_UART_DRAIN claim=[0-9]* vector=0x24 bytes=1 first=0x4[1-8] iir_before=0x04 iir_after=0x01 lsr_dr_after=0 self_irr=0 ' "$NORM")
delivered=$(count 'IRQ1_SPLIT_DELIVERY cpu=0 line=4 outcome=delivered')
probes=$(count 'IRQ1_NOTIFICATION_PROBE_DELIVERED tid=1 notification=0 generation=1 label=4')
completes=$(grep -a -c 'IRQ3_UART_COMPLETE n=[0-9]* vector=0x24 claims=[0-9]* eoi_writes=1 lapic_isr_after=0 remote_irr=0 delivery_pending=0 ' "$NORM")
for v in entries in_service drains1 delivered probes completes; do
  [[ "${!v}" == "8" ]] || die "$v=${!v}, expected 8"
done
drained_bytes=$(grep -a 'IRQ3_UART_DRAIN ' "$NORM" | sed 's/.* first=0x\([0-9a-f]*\) .*/\1/' | tr '\n' ' ')
[[ "$drained_bytes" == "41 42 43 44 45 46 47 48 " ]] || die "drained byte sequence is '$drained_bytes'"
n=$(count 'outcome=no_route'); [[ "$n" == "0" ]] || die "outcome=no_route appeared $n times"
# Per claim, in order: entry -> drain -> delivery -> completion, and never a second of any.
seq_ok=$(grep -a -o 'IRQ3_UART_IRQ_ENTRY\|IRQ3_UART_DRAIN\|IRQ1_SPLIT_DELIVERY cpu=0 line=4\|IRQ3_UART_COMPLETE' "$NORM" \
  | awk 'BEGIN{e="IRQ3_UART_IRQ_ENTRY";d="IRQ3_UART_DRAIN";v="IRQ1_SPLIT_DELIVERY cpu=0 line=4";c="IRQ3_UART_COMPLETE";want=e;n=0;ok=1}
         {if($0!=want){ok=0;exit} if(want==e)want=d; else if(want==d)want=v; else if(want==v)want=c; else {want=e;n++}}
         END{print (ok && want==e) ? n : -1}')
[[ "$seq_ok" == "8" ]] || die "claim/drain/delivery/completion are not 8 ordered quadruples ($seq_ok)"
[[ $(count 'IRQ3_UART_SOURCE_DISABLED vector=0x24 claims=8 data_claims=8 completions=8 completed_clean=8 bytes=8 ier_after=0x0 redir_masked_after=1') == 1 ]] \
  || die "source not disabled after 8 clean completions"
[[ $(count 'IRQ3_UART_TOTALS empty_drains=0 idle_origin=4 user_origin=4 other_origin=0 together_held=2') == 1 ]] \
  || die "fixture totals not 0 empty / 4 idle / 4 user / 0 other / 2 together"

# ── TOGETHER ────────────────────────────────────────────────────────────────────────────────
together=$(grep -a -c 'IRQ3_UART_TOGETHER claim=[0-9]* data_item=[34] origin=\(idle\|user\) timer_irr=1 device_isr=1 ' "$NORM")
[[ "$together" == "2" ]] || die "timer and device were pending together on $together/2 items"
[[ $(count 'IRQ3_UART_TOGETHER claim=3 data_item=3 origin=idle') == 1 && $(count 'IRQ3_UART_TOGETHER claim=4 data_item=4 origin=user') == 1 ]] \
  || die "the together items are not one idle and one user claim"

# ── ORIGINS ─────────────────────────────────────────────────────────────────────────────────
idle_n=$(grep -a -c 'IRQ3_UART_IRQ_ENTRY origin=idle n=[0-9]* claim=[0-9]* vector=0x24 line=4 cs=0x8 rip=0x[0-9a-f]* parked=1 tid=0 ' "$NORM")
user_n=$(grep -a -c 'IRQ3_UART_IRQ_ENTRY origin=user n=[0-9]* claim=[0-9]* vector=0x24 line=4 cs=0x23 rip=0x[0-9a-f]* parked=0 tid=1 ' "$NORM")
other_n=$(count 'IRQ3_UART_IRQ_ENTRY origin=other')
[[ "$idle_n" == "4" && "$user_n" == "4" && "$other_n" == "0" ]] || die "origins idle=$idle_n user=$user_n other=$other_n, expected 4/4/0"
rips() { grep -a "IRQ3_UART_IRQ_ENTRY origin=$1" "$NORM" | sed 's/.* rip=0x\([0-9a-f]*\).*/\1/' | while read -r h; do echo $((16#$h)); done; }
sym_range() { # elf symbol-substring -> "start end" (decimal)
  local addr size
  read -r addr size _ < <(nm -S -C "$1" 2>/dev/null | grep -F -- "$2" | head -1)
  [[ -n "${addr:-}" && -n "${size:-}" ]] || { echo "0 0"; return; }
  echo "$((16#$addr)) $((16#$addr + 16#$size))"
}
at_hlt=0; in_spin=0
if [[ -f "$KELF_COPY" ]]; then
  read -r ps pe < <(sym_range "$KELF_COPY" "descriptor_tables::x86_idle_park_loop")
  (( pe > ps )) || die "x86_idle_park_loop not found in the kernel ELF"
  # The loop's only halt is `sti; hlt`: `sti` defers interrupts by one instruction, so a wake is
  # taken at the `hlt` and its saved RIP is the instruction after it.
  # The compiler may duplicate the loop body; every `hlt` in it must directly follow a `sti`, and
  # every idle RIP must be the instruction after one of them.
  dis_park=$(objdump -d --no-show-raw-insn --start-address="$ps" --stop-address="$pe" "$KELF_COPY")
  hlt_total=$(grep -c -P ':\thlt' <<<"$dis_park")
  hlt_nexts=$(awk '/:\t*sti/{s=1;next} s&&/:\t*hlt/{sub(":","",$1); print $1} {s=0}' <<<"$dis_park" \
    | while read -r h; do echo $((16#$h + 1)); done | tr '\n' ' ')
  n_pairs=$(wc -w <<<"$hlt_nexts")
  (( n_pairs >= 1 && n_pairs == hlt_total )) || die "idle loop halts: $hlt_total hlt, $n_pairs directly after sti"
  while read -r pc; do [[ " $hlt_nexts " == *" $pc "* ]] && at_hlt=$((at_hlt + 1)); done < <(rips idle)
  [[ "$at_hlt" == "$idle_n" ]] || die "idle-origin RIP not the instruction after an idle sti;hlt ($at_hlt/$idle_n)"
else
  die "kernel ELF copy missing; cannot place idle RIP"
fi
if [[ -f "$UELF_COPY" ]]; then
  read -r as ae < <(sym_range "$UELF_COPY" "uart_irq_witness::announce_ready_and_spin")
  read -r ss se < <(sym_range "$UELF_COPY" "uart_irq_witness::spin_checking_registers")
  while read -r pc; do
    if (( (pc >= as && pc < ae) || (pc >= ss && pc < se) )); then in_spin=$((in_spin + 1)); fi
  done < <(rips user)
  (( in_spin >= 1 )) || die "no user-origin interrupt landed inside the register-checked spin"
fi
# Every idle claim leaves through the idle path (never an iretq into the halted loop).
reparks=$(awk '/IRQ3_UART_COMPLETE /{w=1;next} w&&NF{if($0 ~ /^SCHED_ENTER_IDLE_HLT cpu=0/)n++; w=0} END{print n+0}' "$NORM")
[[ "$reparks" == "$idle_n" ]] || die "idle claims re-parked through the idle path $reparks/$idle_n times"
n=$(count 'EXIT_TASK_OWNER_REVALIDATED'); [[ "$n" == "0" ]] || die "owner revalidation ran $n times"

# ── ORDER ───────────────────────────────────────────────────────────────────────────────────
injects=$(grep -a -c '^#HOST_INJECT byte=0x4[1-8] .* seq=' "$NORM")
[[ "$injects" == "8" ]] || die "expected 8 item injections (got $injects)"
for s in 1 2 3 4 5 6 7 8; do
  inj=$(grep -a -n "^#HOST_INJECT .* seq=$s " "$NORM" | head -1 | cut -d: -f1)
  entry=$(grep -a -n 'IRQ3_UART_IRQ_ENTRY ' "$NORM" | sed -n "${s}p" | cut -d: -f1)
  [[ -n "$inj" && -n "$entry" ]] && (( inj < entry )) || die "item $s: injection does not precede its interrupt"
done
dis=$(line_of 'IRQ3_UART_SOURCE_DISABLED ')
post_ticks=0; post_parks=0
if [[ -z "$dis" ]]; then
  die "source was not disabled after the last item"
else
  post_entries=$(tail -n +"$dis" "$NORM" | grep -a -c 'IRQ3_UART_IRQ_ENTRY\|IRQ1_SPLIT_DELIVERY')
  post_inject=$(tail -n +"$dis" "$NORM" | grep -a -c '^#HOST_INJECT byte=0x5a .*post_disable=1')
  post_ticks=$(tail -n +"$dis" "$NORM" | grep -a -c 'TIMER_SPLIT_IDLE_ADVANCE_COMMITTED\|TIMER_SPLIT_TICK_OK')
  post_parks=$(tail -n +"$dis" "$NORM" | grep -a -c 'U8_RECV_TIMEOUT_SETTLED arch=x86_64 tid=1 ')
  post_init=$(tail -n +"$dis" "$NORM" | grep -a -c 'INIT_IDLE_PARK_BEGIN')
  [[ "$post_entries" == "0" ]] || die "$post_entries interrupt entries/deliveries after the source was disabled"
  [[ "$post_inject" == "1" ]] || die "the post-disable byte was not injected"
  (( post_ticks > 0 )) || die "no timer progress after the disable"
  (( post_parks >= 10 )) || die "the receiver's timed parks did not keep expiring ($post_parks)"
  (( post_init >= 1 )) || die "init did not progress past the witness"
fi

# ── ISOLATION ───────────────────────────────────────────────────────────────────────────────
[[ $(grep -a -c 'PAGE_FAULT_UNHANDLED tid=[0-9]* addr=0xfffffffffec00000 access=Read' "$NORM") == 1 ]] \
  || die "the ring-3 load from the I/O APIC window did not take an unhandled fault at that address"
iso=$(grep -a -m1 'IRQ1_UART_ISOLATION' "$NORM" | sed 's/.*msg=//')
has_all "$iso" window_va=0xfffffffffec00000 load_returned=0 anon_map_over_window_refused=1 result=ok

# ── NOTHING FATAL ───────────────────────────────────────────────────────────────────────────
for bad in 'panicked at' 'KERNEL PANIC' 'x86 trap dispatch failed' 'x86 owner revalidation rollback failed' \
           'IRQ3_UART_ENABLE_DEFERRED' 'IRQ3_UART_ROUTE_REFUSED' 'committed=stranded' '#HOST_ERROR'; do
  n=$(count "$bad"); [[ "$n" == "0" ]] || die "$bad appeared $n times"
done

# ── REPORTED, NOT GRADED ────────────────────────────────────────────────────────────────────
xmm_user=$(grep -a 'IRQ1_UART_SIMD seq=[0-9]* mode=user' "$NORM" | grep -a -v -c 'user_simd_mask=0x0 ')
xmm_idle=$(grep -a 'IRQ1_UART_SIMD seq=[0-9]* mode=idle' "$NORM" | grep -a -v -c 'idle_resume_simd_mask=0x0$')
note "XMM sentinel loss: $xmm_user/4 user items, $xmm_idle/4 idle resumes (pre-existing: no user XMM save across kernel entry)"

if (( fail )); then
  echo "X86_UART_IRQ_WITNESS_SEAL result=fail"
  exit 1
fi
echo "X86_UART_IRQ_WITNESS_SEAL items=8 vector=0x24 line=4 claims=$entries completions=$completes deliveries=$delivered probes=$probes idle=$idle_n user=$user_n idle_at_hlt=$at_hlt user_in_spin=$in_spin together=$together post_ticks=$post_ticks post_parks=$post_parks xmm_user_loss=$xmm_user result=ok"
