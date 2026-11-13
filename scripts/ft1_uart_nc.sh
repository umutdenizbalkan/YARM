#!/usr/bin/env bash
# FUTEX-TIME1 §8 negative control: the UNMODIFIED core smoke grades a recorded passing boot
# replayed by a stand-in `qemu-system-<arch>` — once verbatim (must pass), once with ONE required
# marker byte-split the way the raw UART bytes used to split it (must fail, naming the marker).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
W=$S/ft1-wt; OUT=${UNC_OUT:-$S/ft1-evidence/uart/nc}; mkdir -p $OUT/bin
run() { # run <label> <arch> <smp> <log to replay>
  local label=$1 arch=$2 smp=$3 replay=$4
  cat > $OUT/bin/qemu-system-$arch <<Q
#!/usr/bin/env bash
cat "$replay"
Q
  chmod +x $OUT/bin/qemu-system-$arch
  (cd $W && env PATH="$OUT/bin:$PATH" QEMU_SMP=$smp LOGFILE=$OUT/$label.log scripts/qemu-$arch-core-smoke.sh > $OUT/$label.txt 2>&1)
  local rc=$?
  echo "$label rc=$rc $(grep -a -m2 '\[error\]' $OUT/$label.txt | tr '\n' ' ' | cut -c1-200)"
}
X=${UNC_X86_LOG:?}
A=${UNC_A64_LOG:?}
sed 's/X86_AP_KERNEL_CR3_BEGIN cpu=1/X86_AP_KEiRNEL_CRy3_BEGIN cupu=1/' $X > $OUT/x86_damaged.replay
[ "$(grep -ac X86_AP_KERNEL_CR3_BEGIN $OUT/x86_damaged.replay)" = 0 ] || echo "x86 damage not applied"
M='AARCH64_SPLIT_FINALIZE_OK nr=15'
sed "s/AARCH64_SPLIT_FINALIZE_OK nr=15/AARCH64_SPLIT_FINA3LIZE_OK nr=15/g" $A > $OUT/a64_damaged.replay
echo "aarch64 damaged marker: $M (all occurrences)"
run x86_verbatim x86_64 2 $X
run x86_damaged x86_64 2 $OUT/x86_damaged.replay
run a64_verbatim aarch64 2 $A
run a64_damaged aarch64 2 $OUT/a64_damaged.replay
