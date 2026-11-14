#!/usr/bin/env bash
# Addendum B (stf_schedule.md): LE (riscv64 ecall witness) x3 on B=8062c216 and x3 on F, interleaved. Resumable.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=$(cat $S/stf_F)
B=$(git -C $S/stf-wt rev-parse 8062c216)
OUT=$S/stf-evidence/addB; mkdir -p $OUT; touch $OUT/summary.txt
echo "--- addB start $(date -u +%FT%TZ) F=$F B=$B" >> $OUT/summary.txt
for i in 1 2 3; do for k in B F; do
  L=LEB-$k-$i; grep -q "^$L " $OUT/summary.txt && continue
  [ -e $OUT/$L.txt ] && mv $OUT/$L.txt $OUT/$L.txt.interrupted.$(date +%s) && mv $OUT/$L $OUT/$L.interrupted.$(date +%s) 2>/dev/null
  sha=$([ $k = B ] && echo $B || echo $F); W=$S/stfaddB-wt
  git -C $S/stf-wt worktree remove --force $W 2>/dev/null; rm -rf $W
  git -C $S/stf-wt worktree add -q --detach $W $sha
  t0=$(date +%s)
  (cd $W && env -u CARGO_TARGET_DIR LOGDIR=$OUT/$L timeout 2400 scripts/qemu-riscv-ecall-witness-smoke.sh 2) > $OUT/$L.txt 2>&1
  rc=$?
  cpus=$(for t in 10008 10009 10010 10011 10012 10013; do echo -n "$t:$(grep -a -v USER_LOG $OUT/$L/boot.log 2>/dev/null | grep -a -E "tid=$t([^0-9]|$)" | grep -o 'cpu=[0-9]' | sort | uniq -c | awk '{printf "%s%s/",$2,"x"$1}') "; done)
  echo "$L [$(git -C $W rev-parse --short=8 HEAD) porcelain=$(git -C $W status --porcelain | wc -l)] rc=$rc dur=$(( $(date +%s)-t0 ))s cpus=[$cpus] $(grep -a 'RISCV_ECALL_WITNESS_SEAL' $OUT/$L.txt | tail -1 | cut -c1-200)" >> $OUT/summary.txt
  git -C $S/stf-wt worktree remove --force $W
done; done
echo ADDB_DONE >> $OUT/summary.txt
