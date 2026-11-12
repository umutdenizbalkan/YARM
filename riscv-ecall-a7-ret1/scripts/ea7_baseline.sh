#!/usr/bin/env bash
# F's `ecall_contract` module, verbatim, inserted into B — nothing else changed — and run.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
F=${F:?}; B=${B:?}; OUT=${OUT:-$S/ea7-evidence/baseline}; mkdir -p $OUT
R=$S/ea7rep-wt
[ -d $R ] || git -C $S/ea7-wt worktree add -q --detach $R $B
git -C $R checkout -q -f $B && git -C $R clean -qfd
python3 - "$S/ea7-wt" "$R" <<'PY'
import sys, pathlib
f = pathlib.Path(sys.argv[1], "src/kernel/boot/tests.rs").read_text()
s = f.index("        /// RISCV-ECALL-A7-RET1 — the compiler-visible `ecall` register contract")
e = f.rindex("    }\n\n    mod wait_atomic1 {")
mod = f[s:e]
p = pathlib.Path(sys.argv[2], "src/kernel/boot/tests.rs"); b = p.read_text()
anchor = "    }\n\n    mod wait_atomic1 {"
assert b.count(anchor) == 1
p.write_text(b.replace(anchor, mod + anchor))
print("inserted", len(mod), "bytes")
PY
git -C $R diff --stat > $OUT/harness_apply.txt; git -C $R diff > $OUT/baseline_harness.patch
echo "B=$(git -C $R rev-parse --short=8 HEAD) F=$F" >> $OUT/harness_apply.txt
(cd $R && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$S/ea7nc-target cargo test --lib ecall_contract -- --test-threads=1) > $OUT/baseline_repro_$B.log 2>&1
echo "rc=$? $(grep -a 'test result' $OUT/baseline_repro_$B.log | tail -1)" >> $OUT/harness_apply.txt
grep -aE '^test .*(ok|FAILED)$' $OUT/baseline_repro_$B.log | sed 's/kernel::boot::tests::futex_requeue1::riscv_park_ret1::ecall_contract:://' >> $OUT/harness_apply.txt
git -C $R checkout -q -f $B
