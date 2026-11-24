#!/usr/bin/env bash
# VFS-CORRECT1B — cargo check every feature of the kernel and the server crates, one target per
# feature (the architecture a feature names, else x86_64). Usage: feature_sweep.sh WORKTREE OUTDIR
set -u
W=$1
O=$2
mkdir -p $O
cd $W
XCHK="env -u CARGO_TARGET_DIR cargo check -q -Z build-std=core,alloc,compiler_builtins -Z json-target-spec --no-default-features"
features() { sed -n '/^\[features\]/,/^\[/p' "$1" | grep -o '^[a-z0-9][a-z0-9-]* *=' | tr -d ' =' | grep -v '^default$'; }
target_for() {
  case $1 in
    x86_64-*|x86-*|*-x86_64-*|*-x86-*) echo x86_64 ;;
    aarch64-*|*-aarch64-*) echo aarch64 ;;
    riscv64-*|riscv-*|*-riscv64-*|*-riscv-*) echo riscv64 ;;
    *) echo x86_64 ;;
  esac
}
run() {
  local pkg=$1 what=$2 f=$3 t tj; t=$(target_for $f)
  # The kernel builds for the kernel target; every server builds for the user target.
  if [[ $pkg == yarm ]]; then tj=targets/$t-yarm-none.json; else tj=targets/$t-yarm-user-none.json; fi
  $XCHK --target $tj --features $f -p $pkg $what > $O/$pkg.$f.$t.log 2>&1
  echo "$pkg $f $t rc=$?" | tee -a $O/summary.txt
}
[[ "${SKIP_KERNEL:-0}" == 1 ]] || for f in $(features Cargo.toml); do run yarm "--bin kernel_boot" $f; done
for c in yarm-control-plane-servers yarm-fs-servers yarm-driver-servers yarm-compat-servers yarm-network-servers; do
  for f in $(features crates/$c/Cargo.toml); do run $c --bins $f; done
  rm -rf target/x86_64-yarm-user-none target/aarch64-yarm-user-none target/riscv64-yarm-user-none
done
echo SWEEP_DONE >> $O/summary.txt
