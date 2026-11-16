#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# BL5c / BUILD-GATES1: bind a QEMU run to the exact, checked artifacts it boots.
#
# One implementation (`scripts/artifact-provenance.py`) records what each artifact is and verifies
# a boot set; this library is the shell surface every build and every QEMU launcher uses:
#
#   artifact_cargo_build_bin OUTVAR BIN LOG CARGO_BUILD_CMD...
#       ARTIFACT-CARGO-BIND1: runs ONE `cargo build` (the full command, naming `--bin BIN`) with
#       `--message-format=json-render-diagnostics`, and sets OUTVAR to the executable THAT
#       invocation reported for BIN (`scripts/cargo-artifact.py`), wherever its target directory
#       is: `CARGO_TARGET_DIR`, a config `build.target-dir`, or the standard `./target`. Appends
#       `BIN PATH FRESH SHA256` to this process's Cargo ledger. Diagnostics go to stderr (and LOG
#       when non-empty). Fails closed if the build fails or reports no such executable.
#   artifact_build_kernel   ARCH "FEATURE_ARGS" OUT_ELF [OUT_BOOT]
#       Runs THE kernel build (`cargo build -p yarm --bin kernel_boot` for the port's kernel
#       target and profile, with FEATURE_ARGS verbatim), then `artifact_publish_kernel` of the
#       executable that build reported.
#   artifact_publish_kernel ARCH LINKED_ELF "FEATURE_ARGS" OUT_ELF [OUT_BOOT]
#       Refuses unless LINKED_ELF is, byte for byte, an executable this process's own Cargo
#       builds reported (the ledger): a binary that merely sits at a path is never published.
#       Stages a copy of the linked ELF (and its raw boot image, via objcopy, when OUT_BOOT is
#       given), runs the no-FP check on exactly those staged bytes, and only on a pass publishes
#       them atomically with a provenance record derived from FEATURE_ARGS and the Cargo binding.
#       On any failure the destination and its record are REMOVED, so no stale image stays
#       bootable.
#   artifact_record_initramfs ARCH CPIO "FEATURE_ARGS" TARGET PROFILE
#       Records an initramfs the caller just packed, with the feature arguments its servers were
#       built with; every packed member named like a binary in the ledger must be that output.
#   artifact_boot_guard ARCH KERNEL INITRAMFS [REQ ...]
#       Verifies the set before QEMU starts and refuses (non-zero) on any mismatch: an artifact
#       without a record, a replaced/stale/swapped image, another arch or source state, a missing
#       or failed no-FP check, or a feature requirement (`kernel=FEATS` exact, `kernel:FEAT`
#       contains; likewise for `initramfs`). Prints one ARTIFACT_BINDING line.
#   artifact_binding_check ARCH STRICT KERNEL INITRAMFS [REQ ...]
#       The core smokes' entry point (BL5c). Since BUILD-GATES1 provenance is mandatory in every
#       mode: STRICT no longer turns a refusal into a warning; it only decides which feature
#       requirements the caller passes.
#
# No function here relabels an artifact: a mismatch is refused, and the only way to a valid
# record is to build the artifact.

_ab_repo() { (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd); }
_ab_py() { python3 -I "$(_ab_repo)/scripts/artifact-provenance.py" "$@"; }
_ab_cargo() { python3 -I "$(_ab_repo)/scripts/cargo-artifact.py" "$@"; }

# ARTIFACT-CARGO-BIND1 — this process's Cargo ledger: one line per executable its builds reported.
# Per process (not exported): a publish binds only outputs of builds this script ran.
if [[ "${_AB_LEDGER_OWNER:-}" != "$$" ]]; then
  ARTIFACT_CARGO_LEDGER="$(mktemp "${TMPDIR:-/tmp}/yarm-cargo-ledger.XXXXXX")"
  _AB_LEDGER_OWNER=$$
fi

artifact_cargo_build_bin() {
  local __out="$1" bin="$2" log="$3"
  shift 3
  local msgs line rc=0 path fresh sha
  msgs="$(mktemp "${TMPDIR:-/tmp}/yarm-cargo-msgs.XXXXXX")"
  if [[ -n "$log" ]]; then
    "$@" --message-format=json-render-diagnostics >"$msgs" 2> >(tee -a "$log" >&2) || rc=$?
  else
    "$@" --message-format=json-render-diagnostics >"$msgs" || rc=$?
  fi
  if (( rc != 0 )); then
    echo "[error] ARTIFACT-CARGO: cargo build for $bin failed (rc=$rc)" >&2
    rm -f "$msgs"
    return 1
  fi
  if ! line="$(_ab_cargo executable --bin "$bin" --messages "$msgs")"; then
    rm -f "$msgs"
    return 1
  fi
  rm -f "$msgs"
  read -r path fresh sha <<<"$line"
  echo "$bin $path $fresh $sha" >>"$ARTIFACT_CARGO_LEDGER"
  echo "ARTIFACT_CARGO_OUTPUT bin=$bin path=$path fresh=$fresh sha256=$sha" >&2
  if [[ -n "$log" ]]; then
    echo "ARTIFACT_CARGO_OUTPUT bin=$bin path=$path fresh=$fresh sha256=$sha" >>"$log"
  fi
  printf -v "$__out" '%s' "$path"
}

# artifact_features_record ARGS... — `default_features=<0|1> features=<a,b,…|none|all>`.
artifact_features_record() { _ab_py features --feature-args="$*"; }

# The kernel target and profile of each port (the artifact builders' defaults).
_ab_kernel_target() {
  case "$1" in
    x86_64) echo "targets/x86_64-yarm-none.json x86-none x86_64-yarm-none 1" ;;
    aarch64) echo "targets/aarch64-yarm-none.json aarch64-none aarch64-yarm-none 1" ;;
    riscv64) echo "riscv64gc-unknown-none-elf release riscv64gc-unknown-none-elf 0" ;;
    *) return 1 ;;
  esac
}

_ab_objcopy() {
  if command -v llvm-objcopy >/dev/null 2>&1; then echo llvm-objcopy
  elif command -v rust-objcopy >/dev/null 2>&1; then echo rust-objcopy
  else return 1; fi
}

_ab_invalidate() { # PATH... — remove artifacts and their records
  local p
  for p in "$@"; do
    if [[ -n "$p" ]]; then rm -f "$p" "$p.provenance" "$p.nofp.txt"; fi
  done
  return 0
}

artifact_publish_kernel() {
  local arch="$1" linked="$2" fargs="$3" out_elf="$4" out_boot="${5:-}"
  local repo spec target profile feats stage_elf stage_boot report oc by
  repo="$(_ab_repo)"
  spec="$(_ab_kernel_target "$arch")" || { echo "[error] ARTIFACT-PUBLISH: unknown arch $arch"; return 1; }
  read -r target profile _ _ <<<"$spec"
  target="${ARTIFACT_KERNEL_TARGET:-$target}"
  profile="${ARTIFACT_KERNEL_PROFILE:-$profile}"
  by="${ARTIFACT_BUILT_BY:-$(basename "$0")}"
  _ab_invalidate "$out_elf" "$out_boot"
  feats="$(_ab_py features --feature-args="$fargs" | sed -n 's/.* features=//p')" || feats=""
  if [[ -z "$feats" ]]; then
    echo "[error] ARTIFACT-PUBLISH: cannot read the feature arguments '$fargs'"
    return 1
  fi
  if [[ "$feats" == "none" ]]; then feats=""; fi
  if [[ ! -s "$linked" ]]; then
    echo "[error] ARTIFACT-PUBLISH: the linked kernel $linked does not exist"
    return 1
  fi
  local binding
  if ! binding="$(_ab_cargo ledger-check --ledger "$ARTIFACT_CARGO_LEDGER" --path "$linked")"; then
    echo "[error] ARTIFACT-PUBLISH: $linked is not an executable this build's cargo invocation produced (ARTIFACT-CARGO-BIND1: a binary that merely sits at a path is not published); nothing was published"
    return 1
  fi
  mkdir -p "$(dirname "$out_elf")"
  if [[ -n "$out_boot" ]]; then mkdir -p "$(dirname "$out_boot")"; fi
  stage_elf="${out_elf}.staging.$$"
  stage_boot="${out_boot:+${out_boot}.staging.$$}"
  report="${out_elf}.nofp.txt.staging.$$"
  cp "$linked" "$stage_elf" || { _ab_invalidate "$stage_elf"; return 1; }
  if [[ -n "$out_boot" ]]; then
    oc="$(_ab_objcopy)" || { echo "[error] ARTIFACT-PUBLISH: no llvm-objcopy/rust-objcopy for the raw image"; rm -f "$stage_elf"; return 1; }
    if ! "$oc" -O binary "$stage_elf" "$stage_boot"; then
      echo "[error] ARTIFACT-PUBLISH: $oc failed on $linked"
      rm -f "$stage_elf" "$stage_boot"
      return 1
    fi
  fi
  if ! "$repo/scripts/check-kernel-nofp.sh" "$arch" "$stage_elf" "$feats" ${stage_boot:+"$stage_boot"} >"$report" 2>&1; then
    cat "$report"
    echo "[error][integrity] KERNEL-NOFP1: the $arch kernel failed the final-image FP/SIMD check; nothing was published"
    rm -f "$stage_elf" "$stage_boot" "$report"
    return 1
  fi
  grep -a "^NOFP_CHECK " "$report"
  mv -f "$stage_elf" "$out_elf"
  if [[ -n "$out_boot" ]]; then mv -f "$stage_boot" "$out_boot"; fi
  mv -f "$report" "${out_elf}.nofp.txt"
  if ! _ab_py record-kernel --arch "$arch" --elf "$out_elf" ${out_boot:+--boot "$out_boot"} \
      --feature-args="$fargs" --target "$target" --profile "$profile" \
      --nofp-report "${out_elf}.nofp.txt" --built-by "$by" --cargo-binding "$binding"; then
    _ab_invalidate "$out_elf" "$out_boot"
    return 1
  fi
  echo "[ok] ARTIFACT-PUBLISH: $arch kernel $(basename "$out_elf")${out_boot:+ + $(basename "$out_boot")} published with provenance ($(_ab_py features --feature-args="$fargs"))"
}

artifact_build_kernel() {
  local arch="$1" fargs="$2" out_elf="$3" out_boot="${4:-}"
  local spec target profile tdir json repo
  repo="$(_ab_repo)"
  spec="$(_ab_kernel_target "$arch")" || { echo "[error] ARTIFACT-BUILD: unknown arch $arch"; return 1; }
  read -r target profile tdir json <<<"$spec"
  local -a z=(-Z "build-std=core,alloc,compiler_builtins,panic_abort")
  if [[ "$json" == 1 ]]; then z+=(-Z json-target-spec); fi
  # The build names its own output (ARTIFACT-CARGO-BIND1); the triple's directory ($tdir) under
  # some target directory is never assumed.
  local linked="" here rc=0
  here="$(pwd)"
  cd "$repo" || return 1
  # shellcheck disable=SC2086
  artifact_cargo_build_bin linked kernel_boot "" \
    cargo build "${z[@]}" --target "$target" --profile "$profile" $fargs -p yarm --bin kernel_boot \
    || rc=$?
  cd "$here" || return 1
  if (( rc != 0 )); then
    echo "[error] ARTIFACT-BUILD: the $arch kernel build failed ($fargs)"
    _ab_invalidate "$out_elf" "$out_boot"
    return 1
  fi
  : "$tdir"
  ARTIFACT_KERNEL_TARGET="$target" ARTIFACT_KERNEL_PROFILE="$profile" \
    artifact_publish_kernel "$arch" "$linked" "$fargs" "$out_elf" "$out_boot"
}

artifact_record_initramfs() {
  local arch="$1" cpio="$2" fargs="$3" target="$4" profile="$5"
  _ab_py record-initramfs --arch "$arch" --cpio "$cpio" --feature-args="$fargs" \
    --target "$target" --profile "$profile" --built-by "${ARTIFACT_BUILT_BY:-$(basename "$0")}" \
    --cargo-ledger "$ARTIFACT_CARGO_LEDGER"
}

artifact_boot_guard() {
  local arch="$1" kernel="$2" initramfs="$3"
  shift 3
  local -a req=()
  local r
  for r in "$@"; do req+=(--require "$r"); done
  _ab_py verify --arch "$arch" --kernel "$kernel" --initramfs "$initramfs" "${req[@]}"
}

artifact_binding_check() {
  local arch="$1" _strict="$2" kernel="$3" initramfs="$4"
  shift 4
  artifact_boot_guard "$arch" "$kernel" "$initramfs" "$@"
}
