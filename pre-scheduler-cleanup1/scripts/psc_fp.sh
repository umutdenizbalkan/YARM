#!/usr/bin/env bash
# PRINTK-FOOTPRINT1 measurement: matched default artifacts, base vs candidate, per ISA.
# Usage: psc_fp.sh <base-wt> <cand-wt> <out-file>
set -u
B=$1; C=$2; OUT=$3
elf() { case $2 in x86_64) echo $1/build-x86_64/kernel_boot.elf;; aarch64) echo $1/build-aarch64/yarm-aarch64.elf;; riscv64) echo $1/build-riscv64/yarm-riscv64.elf;; esac; }
img() { case $2 in x86_64) echo $1/build-x86_64/kernel_boot.elf;; aarch64) echo $1/build-aarch64/yarm-aarch64.bin;; riscv64) echo $1/build-riscv64/yarm-riscv64.bin;; esac; }
sec() { llvm-size -A "$1" | awk -v s="$2" '$1==s{print $2; f=1} END{if(!f) print 0}'; }
load() { llvm-readelf -lW "$1" | python3 -c 'import sys
f=m=0
for l in sys.stdin:
    p=l.split()
    if p and p[0]=="LOAD": f+=int(p[4],16); m+=int(p[5],16)
print(f,m)'; }
{
echo "base=$(git -C $B rev-parse --short=12 HEAD) cand=$(git -C $C rev-parse --short=12 HEAD) cand_porcelain=$(git -C $C status --porcelain --untracked-files=no | wc -l) $(date -u +%FT%TZ)"
printf "%-8s %-16s %12s %12s %10s\n" isa item base cand delta
for a in x86_64 aarch64 riscv64; do
  eb=$(elf $B $a); ec=$(elf $C $a)
  for s in .text .rodata .data .bss; do
    vb=$(sec $eb $s); vc=$(sec $ec $s); printf "%-8s %-16s %12d %12d %+10d\n" $a $s $vb $vc $((vc-vb))
  done
  ib=$(stat -c %s $(img $B $a)); ic=$(stat -c %s $(img $C $a)); printf "%-8s %-16s %12d %12d %+10d\n" $a "boot_image" $ib $ic $((ic-ib))
  read fb mb < <(load $eb); read fc mc < <(load $ec)
  printf "%-8s %-16s %12d %12d %+10d\n" $a "load_filesz" $fb $fc $((fc-fb))
  printf "%-8s %-16s %12d %12d %+10d\n" $a "load_memsz" $mb $mc $((mc-mb))
  for f in $eb $ec; do echo "  sha256 $(sha256sum $f | cut -c1-16) $f"; done
done
} > $OUT
cat $OUT
