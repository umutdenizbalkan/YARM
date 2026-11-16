#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# ARTIFACT-CARGO-BIND1 regression — the published bytes are the CURRENT cargo invocation's output.
#
# Hermetic (a throwaway host-target crate, no network, no QEMU). It plants a STALE binary at the
# standard `./target` path, rebuilds the crate with `CARGO_TARGET_DIR` pointing elsewhere, and
# checks, through the same library functions the QEMU artifact scripts use:
#
#   1. `artifact_cargo_build_bin` returns the executable in the ALTERNATE target directory — the
#      fresh build, not the stale file at the old path — and records it in the ledger;
#   2. the stale binary at the old path is not bound (ledger-check) and `artifact_publish_kernel`
#      refuses it BEFORE staging anything (nothing is published, no record is written);
#   3. without `CARGO_TARGET_DIR` the standard path is used, and is the rebuilt binary;
#   4. `artifact_record_initramfs` refuses a cpio whose packed member is the stale binary, refuses
#      a cpio with no member from this build, and records one that packs the fresh output;
#   5. the boot verifier refuses a kernel record that carries no Cargo binding.
#
# Prints ARTIFACT_CARGO_BIND_TEST result=ok|fail; exit 0 only when every check holds.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/artifact-binding.sh
source "$REPO/scripts/lib/artifact-binding.sh"

T="$(mktemp -d "${TMPDIR:-/tmp}/yarm-cargo-bind-test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
fails=0
ok()   { echo "[ok] $*"; }
bad()  { echo "[fail] $*"; fails=$((fails + 1)); }
sha()  { sha256sum "$1" | cut -d' ' -f1; }

mkdir -p "$T/probe/src"
cat >"$T/probe/Cargo.toml" <<'EOF'
[package]
name = "probe"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "probe"
path = "src/main.rs"

[workspace]
EOF
host_build() { # VAR — build the probe for the host into whatever target dir the env names
  artifact_cargo_build_bin "$1" probe "" cargo build --quiet --manifest-path "$T/probe/Cargo.toml" --bin probe
}

# The stale binary: an older source, built at the standard ./target path.
echo 'fn main() { println!("stale-v1"); }' >"$T/probe/src/main.rs"
stale_out=""
(unset CARGO_TARGET_DIR; cd "$T/probe" && host_build stale_out) >/dev/null 2>&1
STALE="$T/probe/target/debug/probe"
if [[ -x "$STALE" ]] && [[ "$("$STALE")" == "stale-v1" ]]; then ok "stale binary planted at $STALE"; else bad "could not plant the stale binary"; fi
stale_sha="$(sha "$STALE" 2>/dev/null || echo none)"
cp "$STALE" "$T/stale.copy" 2>/dev/null  # step 3 rebuilds the standard path; keep the stale bytes

# 1. A fresh build into an ALTERNATE target directory.
echo 'fn main() { println!("fresh-v2"); }' >"$T/probe/src/main.rs"
: >"$ARTIFACT_CARGO_LEDGER"
out=""
if CARGO_TARGET_DIR="$T/alt" host_build out 2>"$T/build1.err"; then
  if [[ "$out" == "$(realpath "$T/alt")/debug/probe" ]]; then ok "1: the reported executable is in the alternate target dir ($out)"; else bad "1: reported $out, expected $T/alt/debug/probe"; fi
  if [[ "$("$out")" == "fresh-v2" ]]; then ok "1: it is the fresh build"; else bad "1: it is not the fresh build"; fi
  if [[ "$(sha "$out")" != "$stale_sha" ]]; then ok "1: its bytes differ from the stale binary"; else bad "1: same bytes as the stale binary"; fi
  if grep -q "^probe $out [01] $(sha "$out")$" "$ARTIFACT_CARGO_LEDGER"; then ok "1: ledger records it"; else bad "1: ledger does not record it: $(cat "$ARTIFACT_CARGO_LEDGER")"; fi
  if [[ "$("$STALE")" == "stale-v1" ]]; then ok "1: the old path still holds the stale binary (the trap is armed)"; else bad "1: the old path changed"; fi
else
  bad "1: the alternate-target build failed: $(cat "$T/build1.err")"
fi

# 2. The stale binary at the old path is not bound, and is never published.
if _ab_cargo ledger-check --ledger "$ARTIFACT_CARGO_LEDGER" --path "$STALE" >/dev/null; then bad "2: the stale binary is bound"; else ok "2: the stale binary is not bound"; fi
if _ab_cargo ledger-check --ledger "$ARTIFACT_CARGO_LEDGER" --path "$out" >/dev/null; then ok "2: the fresh output is bound"; else bad "2: the fresh output is not bound"; fi
pub="$T/pub/kernel_boot.elf"
mkdir -p "$T/pub"; echo old >"$pub"; echo old >"$pub.provenance"
if msg="$(artifact_publish_kernel riscv64 "$STALE" "--no-default-features" "$pub" 2>&1)"; then
  bad "2: artifact_publish_kernel published the stale binary"
else
  if [[ "$msg" == *"is not an executable this build's cargo invocation produced"* ]]; then ok "2: publish refused the stale binary"; else bad "2: publish failed for another reason: $msg"; fi
fi
if [[ ! -e "$pub" && ! -e "$pub.provenance" ]]; then ok "2: nothing is published and the previous artifact is gone"; else bad "2: an artifact or record remains at $pub"; fi
# A binding mutated after the build (same path, other bytes) is not bound either.
cp "$out" "$T/copy"; printf 'x' >>"$out"
if _ab_cargo ledger-check --ledger "$ARTIFACT_CARGO_LEDGER" --path "$out" >/dev/null; then bad "2: a modified output is still bound"; else ok "2: an output modified after its build is not bound"; fi
cp "$T/copy" "$out"

# 3. The standard path, without CARGO_TARGET_DIR.
std=""
if (unset CARGO_TARGET_DIR; cd "$T/probe" && host_build std && echo "$std" >"$T/std") 2>"$T/build3.err"; then
  std="$(cat "$T/std")"
  if [[ "$std" == "$(realpath "$T/probe/target")/debug/probe" && "$("$std")" == "fresh-v2" ]]; then ok "3: the standard path is used and holds the rebuilt binary"; else bad "3: standard build reported $std"; fi
else
  bad "3: the standard build failed: $(cat "$T/build3.err")"
fi

# 4. The initramfs is bound to this build's outputs, member by member.
mkcpio() { # OUT NAME=FILE...
  python3 -I - "$@" <<'EOF'
import sys
out, members = sys.argv[1], sys.argv[2:]
def hdr(name, size, mode):
    n = name.encode() + b"\0"
    f = [1, mode, 0, 0, 1, 0, size, 0, 0, 0, 0, len(n), 0]
    h = b"070701" + b"".join(b"%08x" % x for x in f) + n
    return h + b"\0" * ((4 - len(h) % 4) % 4)
blob = b""
for m in members:
    name, path = m.split("=", 1)
    data = open(path, "rb").read()
    blob += hdr(name, len(data), 0o100755) + data + b"\0" * ((4 - len(data) % 4) % 4)
blob += hdr("TRAILER!!!", 0, 0)
open(out, "wb").write(blob)
EOF
}
: >"$ARTIFACT_CARGO_LEDGER"
CARGO_TARGET_DIR="$T/alt" host_build out 2>/dev/null
mkcpio "$T/stale.cpio" "sbin/probe=$T/stale.copy" "init=$T/stale.copy"
if artifact_record_initramfs riscv64 "$T/stale.cpio" "--no-default-features" riscv64gc-unknown-none-elf release >"$T/r1" 2>&1; then
  bad "4: an initramfs packing the stale binary was recorded"
else
  if grep -q "packed members differ" "$T/r1"; then ok "4: an initramfs packing the stale binary is refused"; else bad "4: refused for another reason: $(cat "$T/r1")"; fi
fi
mkcpio "$T/none.cpio" "etc/motd=$T/probe/Cargo.toml"
if artifact_record_initramfs riscv64 "$T/none.cpio" "--no-default-features" riscv64gc-unknown-none-elf release >"$T/r2" 2>&1; then
  bad "4: an initramfs with no member from this build was recorded"
else
  if grep -q "no packed member is an output of this build" "$T/r2"; then ok "4: an initramfs with no member from this build is refused"; else bad "4: refused for another reason: $(cat "$T/r2")"; fi
fi
mkcpio "$T/fresh.cpio" "sbin/probe=$out" "init=$out"
if artifact_record_initramfs riscv64 "$T/fresh.cpio" "--no-default-features" riscv64gc-unknown-none-elf release >"$T/r3" 2>&1; then
  if grep -q "^cargo_inputs=sbin/probe:$(sha "$out"):" "$T/fresh.cpio.provenance"; then ok "4: the fresh initramfs is recorded with its bound member"; else bad "4: record lacks the bound member: $(grep cargo_inputs "$T/fresh.cpio.provenance")"; fi
else
  bad "4: the fresh initramfs was refused: $(cat "$T/r3")"
fi

# 5. The boot verifier refuses a kernel record without a Cargo binding.
python3 -I - "$REPO/scripts/artifact-provenance.py" "$T" <<'EOF' >"$T/r5" 2>&1
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("prov", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
t = sys.argv[2]
k = os.path.join(t, "k.elf"); open(k, "wb").write(b"\x7fELF-not-really")
sh = m.sha256(k)
m.write_record(k, dict(yarm_provenance=m.VERSION, role="kernel", arch="riscv64", sha256=sh,
                       elf="k.elf", elf_sha256=sh, nofp_result="pass", nofp_image_sha256=sh,
                       source_state=m.source_state()["source_state"], cargo_feature_args="--no-default-features",
                       default_features="0", features="none", resolved_features="none"))
problems, _, _ = m.verify("riscv64", k, os.path.join(t, "fresh.cpio"), [])
print("\n".join(problems))
sys.exit(0 if any("is not bound to a Cargo output" in p for p in problems) else 1)
EOF
if [[ $? -eq 0 ]]; then ok "5: a kernel record without a Cargo binding is refused at boot"; else bad "5: not refused: $(cat "$T/r5")"; fi

if (( fails == 0 )); then
  echo "ARTIFACT_CARGO_BIND_TEST result=ok"
  exit 0
fi
echo "ARTIFACT_CARGO_BIND_TEST result=fail failures=$fails"
exit 1
