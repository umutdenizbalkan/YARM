#!/usr/bin/env bash
# Package RISCV-ECALL-A7-RET1 evidence: no build outputs, files > 1 MB gzipped, MANIFEST.sha256.
set -eu
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
E=$S/ea7-evidence
P=$S/ea7-evidence-pkg; rm -rf $P; D=$P/riscv-ecall-a7-ret1; mkdir -p $D/scripts
cp_tree() { (cd "$1" && find . -type d \( -name build -o -name 'build-*' -o -name target \) -prune -o -type f ! -name '*.elf' ! -name '*.bin' ! -name '*.cpio' ! -name '*.img' -print0 | tar --null -T - -cf -) | (mkdir -p "$2" && cd "$2" && tar -xf -); }
for d in baseline baseline-live trial uart nc live qual addA addC record-checks; do
  [ -d $E/$d ] && cp_tree $E/$d $D/$d
done
for f in ea7_schedule.md ea7_nc.py ea7_nc_run.sh ea7_baseline.sh ea7_live.sh ea7_matrix.sh ea7_chain.sh ea7_addA.sh ea7_after.sh ea7_clipcmp.sh ea7_lockattr.sh ea7_sections.sh ea7_apscan.sh ea7_uart_nc.sh ea7_hosted_tests.patch ea7_mod.rs ea7_resume.sh ea7_xw2.sh ea7_apscan2.sh ea7_record.md fwa_zscan.sh ea7_pack.sh; do
  [ -f $S/$f ] && cp $S/$f $D/scripts/
done
find $D -type f -size +1M ! -name '*.gz' -exec gzip -9 {} \;
cp $S/ea7_pack_readme.md $D/README.md
(cd $D && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256 && sha256sum -c --quiet MANIFEST.sha256 && echo MANIFEST_OK)
du -sh $D; find $D -type f | wc -l
