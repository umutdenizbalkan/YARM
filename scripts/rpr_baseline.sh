#!/usr/bin/env bash
# RISCV-PARK-RET1 baseline reproduction: the stage's permanent suite, verbatim, on 2a91a6e8, with
# the one bridge step this stage adds stood in for by what 2a91a6e8 does there: nothing.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
B=$S/rprb-wt; O=$S/rpr-evidence/baseline; mkdir -p $O
[ -d $B ] || git -C $S/rpr-wt worktree add -q --detach $B 2a91a6e8
git -C $B checkout -q -f 2a91a6e8 && git -C $B clean -qfd
python3 -I - $B $S/rpr_suite.rs <<'PY'
import sys, pathlib
b = pathlib.Path(sys.argv[1]); suite = open(sys.argv[2]).read()
t = b / "src/kernel/boot/tests.rs"; s = t.read_text()
anchor = "    mod wait_atomic1 {\n"; assert s.count(anchor) == 1
t.write_text(s.replace(anchor, suite + anchor))
tf = b / "src/kernel/trapframe.rs"; s = tf.read_text()
a = "    /// QEMU-SMP1-ACCEPTANCE §1 — name the incarnation whose continuation this frame now holds."
assert s.count(a) == 1
tf.write_text(s.replace(a, "    /// BASELINE HARNESS: 2a91a6e8 commits nothing into the register file.\n    #[cfg(test)]\n    pub(crate) fn commit_riscv_parked_syscall_returns(&mut self) {}\n\n" + a))
m = b / "src/kernel/boot/mod.rs"; s = m.read_text()
a = "/// Stage 192B: clear the Yield dispatch deferral for `cpu`.\n"
assert s.count(a) == 1
m.write_text(s.replace(a, "/// BASELINE HARNESS: the 2a91a6e8 bridge has no commit step.\n#[cfg(test)]\npub(crate) fn riscv_commit_final_park_result(\n    _cpu_idx: usize,\n    _frame: &mut crate::kernel::trapframe::TrapFrame,\n) -> bool {\n    false\n}\n\n" + a))
print("harness applied")
PY
git -C $B diff > $O/baseline_harness.patch
(cd $B && CARGO_TARGET_DIR=$S/rprb-target cargo test --lib --features hosted-dev futex_requeue1::riscv_park_ret1 -- --test-threads=1) > $O/baseline_repro_2a91a6e8.log 2>&1
echo "rc=$?" >> $O/baseline_repro_2a91a6e8.log
grep -aE '^test .*(ok|FAILED)$|test result' $O/baseline_repro_2a91a6e8.log | sed 's/kernel::boot::tests::futex_requeue1::riscv_park_ret1:://'
