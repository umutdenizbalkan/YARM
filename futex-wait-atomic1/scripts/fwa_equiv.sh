#!/usr/bin/env bash
# Rebuild the default and strict artifacts at the current head and compare kernel and initramfs
# sha256 with the bindings recorded for 9ce9845f (non-race artifacts must be byte-identical).
set -u
S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad
OLD=$S/fwa-evidence/qual/bindings.txt
bad=0
cmp_one() { # id cmd...
  local id=$1; shift
  local out; out=$("$@" 2>&1 | grep -a ARTIFACT_BINDING | tail -1)
  local ks is ok oi; ks=$(grep -o 'kernel_sha256=[0-9a-f]*' <<<"$out"); is=$(grep -o 'initramfs_sha256=[0-9a-f]*' <<<"$out")
  ok=$(grep "^$id " $OLD | head -1 | grep -o 'kernel_sha256=[0-9a-f]*'); oi=$(grep "^$id " $OLD | head -1 | grep -o 'initramfs_sha256=[0-9a-f]*')
  if [[ -n $ks && $ks == "$ok" && $is == "$oi" ]]; then echo "EQUIV $id identical $ks $is"; else echo "EQUIV $id DIFFERENT new=[$ks $is] old=[$ok $oi]"; bad=1; fi
}
cmp_one D1_build_x86_64 scripts/build-qemu-x86_64-artifacts.sh
cmp_one D2_build_aarch64 scripts/build-qemu-aarch64-artifacts.sh
cmp_one D3_build_riscv64 scripts/build-qemu-riscv64-artifacts.sh
cmp_one S1_build_x86_64_strict env ARTIFACTS_STRICT=1 BOOTSTRAP_FEATURE_ARGS="--no-default-features --features timer-contract-witness" scripts/build-qemu-x86_64-artifacts.sh
cmp_one S3_build_aarch64_strict env ARTIFACTS_STRICT=1 scripts/build-qemu-aarch64-artifacts.sh
cmp_one S5_build_riscv64_strict env ARTIFACTS_STRICT=1 scripts/build-qemu-riscv64-artifacts.sh
[[ $bad == 0 ]] && echo EQUIV_ALL_IDENTICAL
exit $bad
