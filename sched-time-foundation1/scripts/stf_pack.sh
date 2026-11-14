#!/usr/bin/env bash
# Package SCHED-TIME-FOUNDATION1 evidence: no build outputs, files > 1 MB gzipped, MANIFEST.sha256.
set -eu
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
E=$S/stf-evidence
P=$S/stf-evidence-pkg; rm -rf $P; D=$P/sched-time-foundation1; mkdir -p $D/scripts
cp_tree() { (cd "$1" && find . -type d \( -name build -o -name 'build-*' -o -name target \) -prune -o -type f ! -name '*.elf' ! -name '*.bin' ! -name '*.cpio' ! -name '*.img' -print0 | tar --null -T - -cf -) | (mkdir -p "$2" && cd "$2" && tar -xf -); }
cp_tree $E $D/evidence
for f in $(cd $S && ls stf_*.sh stf_*.py stf_*.md stfq_*.sh fwa_zscan.sh lsremote_stf1_*.txt 2>/dev/null); do cp $S/$f $D/scripts/; done
find $D -type f -size +1M ! -name '*.gz' -exec gzip -9 {} \;
cp $S/stf_pack_readme.md $D/README.md
(cd $D && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 sha256sum > MANIFEST.sha256 && sha256sum -c --quiet MANIFEST.sha256 && echo MANIFEST_OK)
du -sh $D; find $D -type f | wc -l
