#!/usr/bin/env bash
# Package RISCV-PARK-RET1 evidence: no build outputs, files > 1 MB gzipped, MANIFEST.sha256.
set -eu
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
E=$S/rpr-evidence
P=$S/rpr-evidence-pkg; rm -rf $P; D=$P/riscv-park-ret1; mkdir -p $D/scripts
cp_tree() { (cd "$1" && find . -type d \( -name build -o -name 'build-*' -o -name target \) -prune -o -type f ! -name '*.elf' ! -name '*.bin' ! -name '*.cpio' ! -name '*.img' -print0 | tar --null -T - -cf -) | (mkdir -p "$2" && cd "$2" && tar -xf -); }
for d in baseline baseline-live nc live qual a7scan trial-riscv64 trial2 footprint record-commit-checks; do
  [ -d $E/$d ] && cp_tree $E/$d $D/$d
done
for f in rpr_nc.py rpr_nc_run.sh rpr_baseline.sh rpr_live.sh rpr_matrix.sh rpr_chain.sh rpr_addA.sh rpr_addB.sh rpr_addC.sh rpr_fill.py rpr_textcmp.sh rpr_clipcmp.sh rpr_lockattr.sh rpr_a7scan.py rpr_witness_kernel.py rpr_schedule.md fwa_zscan.sh rpr_pack.sh; do
  [ -f $S/$f ] && cp $S/$f $D/scripts/
done
find $D -type f -size +1M ! -name '*.gz' -exec gzip -9 {} \;
cp $S/rpr_pack_readme.md $D/README.md
(cd $D && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256 && sha256sum -c --quiet MANIFEST.sha256 && echo MANIFEST_OK)
du -sh $D; find $D -type f | wc -l
