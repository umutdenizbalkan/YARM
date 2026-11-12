#!/usr/bin/env bash
# AP: secondary-CPU startup and marker integrity. For every log with x86_64 AP bring-up and every
# AArch64 log: byte-split X86_AP_* marker names, Rust-entry ladder bytes on COM1 ('@HVW', 'kcX86'),
# APs online, and AArch64 raw TTBR0 bytes ('3ASW3', '4ASW4'). F (matrix logs) and B (B's logs).
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
Q=${OUT:-$S/ea7-evidence/qual}
bad=0
scan() { # scan <side> <files...>
  local side=$1; shift
  for f in "$@"; do
    [ -f "$f" ] || continue
    if grep -aq 'X86_AP_ONLINE' "$f"; then
      split=$(grep -aoE 'X86_A?P?_?[A-Z0-9_]*[a-z][A-Za-z0-9_]*' "$f" | grep -c 'X86_AP')
      ladder=$(grep -acE '@HVW|kcX86' "$f")
      echo "x86 $side split=$split rust_ladder_on_com1=$ladder ap_online=$(grep -ac X86_AP_ONLINE "$f") $f"
      [ $side = F ] && [ $((split + ladder)) -gt 0 ] && bad=1
    fi
    if grep -aq 'ASW3 immediately after msr ttbr0_el1' "$f"; then
      raw=$(grep -acE '3ASW3 |4ASW4 ' "$f")
      echo "a64 $side raw_ttbr_bytes=$raw asw3=$(grep -ac 'ASW3 immediately' "$f") $f"
      [ $side = F ] && [ $raw -gt 0 ] && bad=1
    fi
  done
}
F_LOGS=$(find $Q -type f \( -name '*.log' -o -name '*.norm.log' \) ! -path '*/C4_*' ! -path '*/FB*' ! -name '*_B.log' ! -path '*_B/*' ! -path '*/H6_*' ! -name 'H*' 2>/dev/null)
B_LOGS=$(find $Q -type f -name '*.log' \( -path '*/C4_*' -o -path '*/FB*' -o -name '*_B.log' -o -path '*_B/*' \) 2>/dev/null)
scan F $F_LOGS
scan B $B_LOGS
[ $bad = 0 ] && echo AP_MARKER_INTEGRITY_OK
exit $bad
