#!/bin/bash
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
cd $S/fst1-wt
O=$S/fs1-dev
env -u CARGO_TARGET_DIR TIMEOUT_SECS=360 SEAL_CELLS="os nr17 lh pb rb" SEAL_PROFILE=rtb LOGDIR=$O/full3-riscv64-rtb scripts/qemu-futex-seal-witness-smoke.sh riscv64 2 > $O/full3-riscv64-rtb.out 2>&1
echo "full3-riscv64-rtb rc=$? $(tail -1 $O/full3-riscv64-rtb.out | cut -c1-300)" >> $O/dev3-summary.txt
P=$(python3 -I $S/fs1-nc/nc.py ncB_condvar_no_reacquire --path)
python3 -I $S/fs1-nc/nc.py ncB_condvar_no_reacquire > $O/ncB-dev.out 2>&1
env -u CARGO_TARGET_DIR TIMEOUT_SECS=360 SEAL_CELLS="os nr17 lh pb rb" SEAL_PROFILE=rtb LOGDIR=$O/ncB-dev-riscv64 scripts/qemu-futex-seal-witness-smoke.sh riscv64 2 >> $O/ncB-dev.out 2>&1
echo "ncB-dev rc=$? $(tail -1 $O/ncB-dev.out | cut -c1-300)" >> $O/dev3-summary.txt
git checkout -- $P
echo "porcelain=$(git status --porcelain | wc -l)" >> $O/dev3-summary.txt
env -u CARGO_TARGET_DIR TIMEOUT_SECS=900 LOGDIR=$O/time-dev-riscv64 scripts/qemu-futex-time-witness-smoke.sh riscv64 2 > $O/time-dev-riscv64.out 2>&1
echo "time-dev-riscv64 rc=$? $(tail -1 $O/time-dev-riscv64.out | cut -c1-200)" >> $O/dev3-summary.txt
echo ALLDONE >> $O/dev3-summary.txt
