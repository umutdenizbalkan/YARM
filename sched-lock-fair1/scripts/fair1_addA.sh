#!/usr/bin/env bash
# SCHED-LOCK-FAIR1 addendum A: read-only analysis of the default kernels the runner built and
# saved (qual/kernels/{B,C}-<port>.elf). Nothing is built or booted.
#   A1 data/bss symbol deltas, LLVM-local suffixes normalised (where the scheduler-state growth
#      lands in the image);
#   A2 the out-of-line 1L lock function on each port, B (TTAS) and C (ticket), or a note that it
#      is inlined at each site;
#   A3 atomic instruction counts over the whole kernel image (x86 lock xadd / lock cmpxchg;
#      RISC-V amoadd.w / lr.w with the A extension enabled for the disassembler).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
K=$S/fair1-evidence/qual/kernels
O=$S/fair1-evidence/qual/addA
mkdir -p $O
echo "addA start $(date -u +%FT%TZ) sha=$(sha256sum $0 | cut -c1-16)" > $O/summary.txt
( cd $K && sha256sum -c SHA256SUMS ) >> $O/summary.txt 2>&1
python3 -I - $K > $O/A1_data_symbol_deltas.txt <<'EOF'
import subprocess, sys, collections, re
d = sys.argv[1]
for t in ("x86_64", "aarch64", "riscv64"):
    syms = {}
    for v in "BC":
        out = subprocess.run(["llvm-nm", "-S", "--defined-only", "--demangle", "%s/%s-%s.elf" % (d, v, t)],
                             capture_output=True, text=True).stdout
        m = collections.Counter()
        for l in out.splitlines():
            p = l.split(None, 3)
            if len(p) == 4 and p[2] in "bBdD":
                m[re.sub(r" \(\.llvm\.\d+\)", "", p[3]).split("::h")[0]] += int(p[1], 16)
        syms[v] = m
    keys = set(syms["B"]) | set(syms["C"])
    diffs = sorted(((syms["C"][k] - syms["B"][k], k) for k in keys if syms["C"][k] != syms["B"][k]),
                   key=lambda x: -abs(x[0]))
    print("== %s: sized data/bss symbols B=%d C=%d delta=%+d" % (
        t, sum(syms["B"].values()), sum(syms["C"].values()),
        sum(syms["C"].values()) - sum(syms["B"].values())))
    for dd, k in diffs[:8]:
        print("  %+8d %s" % (dd, k[:110]))
EOF
echo "A1 rc=$?" >> $O/summary.txt
dis() {
  local f=$1 pat=$2 line a s
  line=$(llvm-nm --demangle -S "$f" | grep -E "$pat" | head -1)
  [[ -z "$line" ]] && { echo "  (no out-of-line symbol: inlined at each site)"; return; }
  a=$(echo $line | cut -d' ' -f1); s=$(echo $line | cut -d' ' -f2)
  echo "  [$a size 0x$s]"
  llvm-objdump -d --mattr=+a,+c,+m --no-show-raw-insn --start-address=0x$a \
    --stop-address=$(printf '0x%x' $((0x$a + 0x$s))) "$f" | grep -E "^ *[0-9a-f]+:" | sed -E 's/^ *[0-9a-f]+:\s*/  /'
}
for t in x86_64 aarch64 riscv64; do
  echo "##### $t B: SpinLockIrq<CpuSched>::lock"; dis $K/B-$t.elf 'SpinLockIrq<yarm::kernel::scheduler::CpuSched>>::lock$'
  echo "##### $t C: TicketLockIrq<CpuSched>::lock"; dis $K/C-$t.elf 'TicketLockIrq<yarm::kernel::scheduler::CpuSched>>::lock$'
done > $O/A2_lock_disassembly.txt 2>&1
echo "A2 rc=$?" >> $O/summary.txt
for v in B C; do
  x=$(llvm-objdump -d --no-show-raw-insn $K/$v-x86_64.elf)
  r=$(llvm-objdump -d --mattr=+a,+c,+m --no-show-raw-insn $K/$v-riscv64.elf)
  echo "$v x86_64 lock_xadd=$(grep -c 'xadd' <<<"$x") lock_cmpxchg=$(grep -c 'cmpxchg' <<<"$x") | riscv64 amoadd.w=$(grep -cE 'amoadd\.w' <<<"$r") lr.w=$(grep -cE '\blr\.w' <<<"$r")"
done > $O/A3_atomic_counts.txt 2>&1
echo "A3 rc=$?" >> $O/summary.txt
echo ADDA_DONE >> $O/summary.txt
