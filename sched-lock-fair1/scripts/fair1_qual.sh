#!/usr/bin/env bash
# SCHED-LOCK-FAIR1 focused qualification: candidate C vs baseline dce48596 (B). Steps in order,
# every log kept, nothing rerun in place. Unrelated coverage reuses the accepted SCHED-LOCK2B
# evidence; no historical matrix.
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
C=${C:?candidate}
B=dce48596
O=$S/fair1-evidence/qual
W=$S/fair1-wt
BW=$S/sl2b-base-wt
SUM=$O/summary.txt
mkdir -p $O/kernels
step() {
  local name=$1; shift
  local por; por=$(git -C $W status --porcelain | wc -l)
  echo "=== $name [$(git -C $W rev-parse --short=12 HEAD) porcelain=$por]: $* (start $(date -u +%H:%M:%S))" >> $SUM
  local t0=$SECONDS
  ( cd $W && "$@" ) > $O/$name.log 2>&1
  local rc=$?
  echo "$name rc=$rc secs=$((SECONDS - t0))" >> $SUM
}
pairs() { awk '{ while (match($0, /YARM_LOCK_ORDER_WARN current=[a-z_]+ previous=[0-9]+/)) { w=substr($0, RSTART, RLENGTH); split(w, a, " "); c[a[2]" "a[3]]++; $0=substr($0, RSTART+RLENGTH) } } END { for (k in c) print k, c[k] }' | sort; }
kpath() { grep -a "ARTIFACT_CARGO_OUTPUT bin=kernel_boot " "$1" | tail -1 | sed 's/.* path=\([^ ]*\) .*/\1/'; }
echo "fair1 qual start $(date -u +%FT%TZ) C=$C B=$B sha=$(sha256sum $0 | cut -c1-16)" >> $SUM
git -C $W checkout -q --detach $C
git -C $BW checkout -q --detach $B
[[ "$(git -C $BW rev-parse --short=8 HEAD)" == "$B" ]] || { echo "baseline worktree not at $B" >> $SUM; exit 1; }

# ── hosted ──────────────────────────────────────────────────────────────────────────────────
step H1_fmt cargo fmt --all -- --check
step H2_hosted_lib env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1
step H3_integration env -u CARGO_TARGET_DIR cargo test -q --tests --no-fail-fast -- --test-threads=1
step H3b_grader_cli env -u CARGO_TARGET_DIR cargo test -q --test sched_lock1_grader_cli -- --test-threads=1
for f in fair1_ sched_lock1 sl1_ lock_order census abi stage199d u3_ed kthread apsched ap_sched1 sl2b_ sl2a_ task_enqueue the_denial_is_documented overtaken reap futex ipc_timeout sr_place1; do
  step H4_subset_$f env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 $f
done
# lock-order: descending sequential pair TYPES, candidate vs baseline, whole serial lib suite
step H5_lockorder_C bash -c "env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_C.raw.log 2>&1; echo rc=\$?"
step H5_lockorder_B bash -c "cd $BW && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 --nocapture > $O/H5_B.raw.log 2>&1; echo rc=\$?"
pairs < $O/H5_C.raw.log > $O/H5_C.pairs.txt; pairs < $O/H5_B.raw.log > $O/H5_B.pairs.txt
diff <(awk '{print $1, $2}' $O/H5_C.pairs.txt) <(awk '{print $1, $2}' $O/H5_B.pairs.txt) > $O/H5_pairtypes.diff; echo "H5_pairtypes C=$(wc -l < $O/H5_C.pairs.txt) B=$(wc -l < $O/H5_B.pairs.txt) diff_lines=$(wc -l < $O/H5_pairtypes.diff)" >> $SUM
gzip -f $O/H5_C.raw.log $O/H5_B.raw.log
step G1_contract_gate bash scripts/check-current-contracts.sh
step G2_doc_enforcement bash scripts/check-contract-doc-enforcement.sh
# cross-target checks of the witness feature (the default builds follow)
for a in x86_64:x86 aarch64:aarch64 riscv64:riscv64; do
  t=${a%:*}; f=${a#*:}
  step X_check_witness_$t env -u CARGO_TARGET_DIR cargo check -q --target targets/$t-yarm-none.json -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features --features $f-sched-lock1-witness -p yarm --bin kernel_boot
done

# ── scheduler-state sizes (hosted layout; same 64-bit layout on every port), C and B ────────
PROBE='
#[cfg(test)]
mod fair1_size_probe {
    #[test]
    fn fair1_size_probe() {
        std::println!(
            "FAIR1_SIZE CpuSched={} CpuCell={} Admission={} SchedDomain={} align_CpuCell={}",
            core::mem::size_of::<super::CpuSched>(),
            core::mem::size_of::<super::CpuCell>(),
            core::mem::size_of::<super::Admission>(),
            core::mem::size_of::<super::SchedDomain>(),
            core::mem::align_of::<super::CpuCell>()
        );
    }
}'
step Z1_size_probe_C bash -c "printf '%s\n' \"\$1\" >> src/kernel/scheduler.rs && env -u CARGO_TARGET_DIR cargo test -q --lib fair1_size_probe -- --nocapture --test-threads=1; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc" _ "$PROBE"
step Z1_size_probe_B bash -c "cd $BW && printf '%s\n' \"\$1\" >> src/kernel/scheduler.rs && env -u CARGO_TARGET_DIR cargo test -q --lib fair1_size_probe -- --nocapture --test-threads=1; rc=\$?; git checkout -- src/kernel/scheduler.rs; exit \$rc" _ "$PROBE"

# ── default builds, C then B (matched profile: each port's own artifact script) ───────────────
for t in x86_64 aarch64 riscv64; do
  step B_build_C_$t env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh
  k=$(kpath $O/B_build_C_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/C-$t.elf
  step B_build_B_$t bash -c "cd $BW && env -u CARGO_TARGET_DIR scripts/build-qemu-$t-artifacts.sh"
  k=$(kpath $O/B_build_B_$t.log); [[ -n "$k" ]] && cp "$k" $O/kernels/B-$t.elf
done
( cd $O/kernels && sha256sum *.elf > SHA256SUMS
  for t in x86_64 aarch64 riscv64; do
    for v in B C; do echo "== $v-$t"; llvm-size -A $v-$t.elf | grep -E '^\.(text|rodata|data|bss) '; done
  done ) > $O/Z2_sizes.txt 2>&1
echo "Z2_sizes written" >> $SUM
step Z3_xadd_sites bash -c "for v in B C; do echo \"\$v lock-xadd=\$(objdump -d --no-show-raw-insn $O/kernels/\$v-x86_64.elf | grep -c 'lock xadd') lock-cmpxchg=\$(objdump -d --no-show-raw-insn $O/kernels/\$v-x86_64.elf | grep -c 'lock cmpxchg')\"; done"

# ── live: the SCHED-LOCK1 witness, extended, SMP2 on each port ────────────────────────────────
step W0_grader_self_test python3 -I scripts/grade-sched-lock1-witness.py --self-test
step W1_x86_64_smp2 env -u CARGO_TARGET_DIR ARCH=x86_64 SMP=2 LOGDIR=$O/W1 scripts/qemu-sched-lock1-witness-smoke.sh
step W2_aarch64_smp2 env -u CARGO_TARGET_DIR ARCH=aarch64 SMP=2 LOGDIR=$O/W2 scripts/qemu-sched-lock1-witness-smoke.sh
step W3_riscv64_smp2 env -u CARGO_TARGET_DIR ARCH=riscv64 SMP=2 LOGDIR=$O/W3 scripts/qemu-sched-lock1-witness-smoke.sh

# ── negative controls (expected rc != 0; tree restored after each) ───────────────────────────
# NC1: interrupts masked only AFTER the ticket is drawn.
step NC1_mask_after_draw bash -c "python3 -c \"
p='src/kernel/ticket_lock.rs'; s=open(p).read()
a='''        let irq_state = irq_save();
        // (2) Draw'''
d='''        let ticket = self.tickets.next.fetch_add(1, Ordering::Relaxed);'''
assert s.count(a)==1 and s.count(d)==1
s=s.replace(a,'''        // (2) Draw''').replace(d, d+'''
        let irq_state = irq_save();''')
open(p,'w').write(s)
\" && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 fair1_interrupts; rc=\$?; git checkout -- src/kernel/ticket_lock.rs; exit \$rc"
# NC2: a try-lock that takes the next ticket even while the lock is held (barging).
step NC2_try_lock_barges bash -c "python3 -c \"
p='src/kernel/ticket_lock.rs'; s=open(p).read()
a='''        let serving = self.tickets.serving.load(Ordering::Acquire);
        if self'''
assert s.count(a)==1
open(p,'w').write(s.replace(a,'''        let serving = self.tickets.next.load(Ordering::Acquire);
        if self'''))
\" && git diff --stat && env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 fair1_try_lock; rc=\$?; git checkout -- src/kernel/ticket_lock.rs; exit \$rc"
# NC3: a barging (TTAS-like) acquisition in place of the ticket draw — mutual exclusion kept,
# FIFO lost. Hosted fair1_ checks, then the live x86_64 witness, must both reject it.
BARGE="python3 -c \"
p='src/kernel/ticket_lock.rs'; s=open(p).read()
d='''        let ticket = self.tickets.next.fetch_add(1, Ordering::Relaxed);'''
assert s.count(d)==1
open(p,'w').write(s.replace(d,'''        let ticket = loop {
            let s = self.tickets.serving.load(Ordering::Acquire);
            if self.tickets.next.compare_exchange(s, s.wrapping_add(1), Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                break s;
            }
            spin_loop();
        };'''))
\""
step NC3a_barging_lock_hosted bash -c "$BARGE && git diff --stat && timeout 1800 env -u CARGO_TARGET_DIR cargo test -q --lib -- --test-threads=1 fair1_; rc=\$?; git checkout -- src/kernel/ticket_lock.rs; exit \$rc"
step NC3b_barging_lock_live_x86_64 bash -c "$BARGE && git diff --stat && env -u CARGO_TARGET_DIR TIMEOUT_SECS=2400 ARCH=x86_64 SMP=2 LOGDIR=$O/NC3b scripts/qemu-sched-lock1-witness-smoke.sh; rc=\$?; git checkout -- src/kernel/ticket_lock.rs; exit \$rc"
# NC4: the grader on W1's own live log, mutated (CRC recomputed): (a) the rapid re-acquire of
# 1L[0] drew the peer's ticket, i.e. overtook it; (b) a service gap on 1L[1].
MUT="$S/fair1_mutlog.py"
step NC4a_live_log_reacquire_overtakes bash -c "python3 -I $MUT $O/W1/boot.norm.log $O/NC4a.log reacq && python3 -I scripts/grade-sched-lock1-witness.py --expect-cpus 2 $O/NC4a.log"
step NC4b_live_log_service_gap bash -c "python3 -I $MUT $O/W1/boot.norm.log $O/NC4b.log gap && python3 -I scripts/grade-sched-lock1-witness.py --expect-cpus 2 $O/NC4b.log"
echo "post-NC porcelain=$(git -C $W status --porcelain | wc -l) base-porcelain=$(git -C $BW status --porcelain | wc -l)" >> $SUM
echo "fair1 qual end $(date -u +%FT%TZ)" >> $SUM
echo FAIR1_QUAL_DONE >> $SUM
