#!/usr/bin/env bash
# Package FUTEX-WAIT-ATOMIC1 evidence: no build outputs, files > 1 MB gzipped, MANIFEST.sha256.
set -eu
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
E=$S/fwa-evidence
P=$S/fwa-evidence-pkg; rm -rf $P; D=$P/futex-wait-atomic1; mkdir -p $D/scripts
cp_tree() { (cd "$1" && find . -type d \( -name build -o -name 'build-*' -o -name target \) -prune -o -type f ! -name '*.elf' ! -name '*.bin' ! -name '*.cpio' ! -name '*.img' -print0 | tar --null -T - -cf -) | (mkdir -p "$2" && cd "$2" && tar -xf -); }
for d in nc nc-prelim live live-trial-prefreeze qual qual-ae65c57e live-ae65c57e diag-baseline-parked-return equiv-sections record-commit-checks patches; do
  [ -d $E/$d ] && cp_tree $E/$d $D/$d
done
for f in baseline_harness.patch baseline_harness_apply.txt baseline_repro_c2b9893e.log contract_gate_F.log contract_gate_controls.log regrade_supplementary.txt fwa_hook.py; do
  cp $E/$f $D/
done
for f in fwa_nc.py fwa_nc_run.sh fwa_baseline.sh fwa_hook.py fwa_live.sh fwa_live2.sh fwa_matrix.sh fwa_zscan.sh fwa_clipcmp.sh fwa_lockattr.sh fwa_equiv.sh fwa_rerun.sh fwa_chain.sh fwa_schedule.md fwa_regrade.py fwa_pack.sh; do
  [ -f $S/$f ] && cp $S/$f $D/scripts/
done
find $D -type f -size +1M ! -name '*.gz' -exec gzip -9 {} \;
cat > $D/README.md <<'R'
# FUTEX-WAIT-ATOMIC1 evidence

Raw logs behind the FUTEX-WAIT-ATOMIC1 stage record (`doc/KERNEL_UNLOCKING.md`). Frozen candidate
F = `9ce9845f`; baseline B = `c2b9893e`. The record commit `2a91a6e8` adds only the record and two
entries in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`.

| path | contents |
|---|---|
| `baseline_repro_c2b9893e.log`, `baseline_harness.patch` | F's `wait_atomic1` suite run against B with only the test hooks added: w01b, w04, w06, w16 fail, the other 17 pass |
| `contract_gate_F.log`, `contract_gate_controls.log` | `check-contract-doc-enforcement.sh` on F (rc 0) and its five mutation controls (each rc 1) |
| `nc/` | the 11 hosted negative controls at F (filter `futex`, 216 tests): patch, log, `summary.txt`; and the unmodified control |
| `nc-prelim/` | an earlier control run at `4583533f`, kept; not evidence for F |
| `live-trial-prefreeze/` | witness trials before freezing |
| `live/` | at F: the wait witness ×3 (RISC-V failed: witness placement) and the live NC1 on x86_64 (fails, as it must) |
| `qual/` | the full predeclared matrix at F against B (`scripts/fwa_schedule.md`, run by `scripts/fwa_matrix.sh`), `summary.txt`, `bindings.txt`, `H6_attribution/` |
| `qual-ae65c57e/`, `live-ae65c57e/` | the appended run at the witness-only candidate `ae65c57e` (declared in the schedule before any matrix result was used): hosted group, gates, wait witness ×3, live NC1, and `E_equiv.log` (artifacts NOT identical, so `ae65c57e` is not delivered) |
| `equiv-sections/` | why E differs: x86_64 default kernel, `.text` identical, one `.rodata` word differs by +24 (a panic-location line) |
| `diag-baseline-parked-return/` | B plus only the userspace witness (`patches/diag-baseline-51198a57`): RISC-V 16/16 parked waits return `Err`, AArch64 0/16 — the pre-existing RISC-V return-lane defect |
| `regrade_supplementary.txt` | every wait-witness log re-graded with damaged marker prefixes repaired (supplementary, not a gate) |
| `record-commit-checks/` | contract gate, hosted lib and integration on the record commit's tree |
| `patches/witness-fix-ae65c57e/` | the undelivered witness fix |
| `scripts/` | runners, the control patcher, the schedule, the packer |

Build outputs are excluded; files over 1 MB are gzipped. `MANIFEST.sha256` covers every other file.
R
(cd $D && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256 && sha256sum -c --quiet MANIFEST.sha256 && echo MANIFEST_OK)
du -sh $D; find $D -type f | wc -l
