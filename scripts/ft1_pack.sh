#!/usr/bin/env bash
# Package FUTEX-TIME1 evidence: no build outputs, files > 1 MB gzipped, MANIFEST.sha256.
set -eu
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
E=$S/ft1-evidence
P=$S/ft1-evidence-pkg; rm -rf $P; D=$P/futex-time1; mkdir -p $D/scripts
cp_tree() { (cd "$1" && find . -type d \( -name build -o -name 'build-*' -o -name target \) -prune -o -type f ! -name '*.elf' ! -name '*.bin' ! -name '*.cpio' ! -name '*.img' -print0 | tar --null -T - -cf -) | (mkdir -p "$2" && cd "$2" && tar -xf -); }
for d in nc nc-Fpp live qual addC addE addF featsweep-Fprime featsweep-B metrics record-checks; do
  [ -d $E/$d ] && cp_tree $E/$d $D/$d
done
for f in featsweep-compare.txt fprime_hosted_lib.log predeclared.sha; do cp $E/$f $D/; done
cp_tree $S/ft1-live $D/dev
for f in $(cd $S && ls ft1_*.sh ft1_*.py ft1_*.md ft1c_*.py ft1c_*.md fwa_zscan.sh 2>/dev/null); do cp $S/$f $D/scripts/; done
find $D -type f -size +1M ! -name '*.gz' -exec gzip -9 {} \;
cp $S/ft1_pack_readme.md $D/README.md
(cd $D && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256 && sha256sum -c --quiet MANIFEST.sha256 && echo MANIFEST_OK)
du -sh $D; find $D -type f | wc -l
