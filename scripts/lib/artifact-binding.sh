#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# BL5c: bind a QEMU run to the exact artifacts it boots.
#
# A core smoke used to boot whatever its KERNEL_IMAGE / INITRAMFS_IMAGE paths held. A strict run
# whose artifacts lacked a witness feature therefore booted, ran to its timeout, and failed late
# on the witness's absence (`timer contract witness absent`) — indistinguishable from the witness
# REGRESSING in an artifact that does carry it. And nothing tied a strict pass to the artifacts it
# judged: a kernel swapped in after the build, or an initramfs from a different build directory,
# was graded as if it were the recorded build.
#
# The build now records, per artifact, its role, its hash and the cargo features it was built
# with (`common_write_manifest`). Before booting, each core smoke calls `artifact_binding_check`:
#
#   * the kernel and the initramfs must both be the artifacts ONE manifest records, by basename and
#     sha256, and the manifest must be for the smoke's arch;
#   * every feature the run's mode requires must be recorded on the artifact that carries it.
#
# A strict run REFUSES before QEMU starts and says why. A non-strict run reports the same binding
# and continues (no feature is required of it). Either way the run prints one ARTIFACT_BINDING line
# naming the kernel, initramfs, hashes, features and manifest it was bound to.
#
# A runner that boots a kernel it built outside the artifact build (an oracle feature kernel) binds
# that set explicitly with `artifact_write_run_manifest`, recording the kernel's own features and
# carrying the initramfs record over from the build that produced it.

# artifact_features_record ARGS... — normalize cargo feature arguments.
# Prints `default_features=<0|1> features=<a,b,…|none|all>`, the list sorted and de-duplicated.
artifact_features_record() {
  local default=1 all=0 want_list=0 tok item
  local -a feats=() more=()
  # Word-split exactly as the build scripts' unquoted `${BOOTSTRAP_FEATURE_ARGS}` is.
  # shellcheck disable=SC2048
  for tok in $*; do
    if (( want_list )); then
      IFS=',' read -r -a more <<<"$tok"
      feats+=("${more[@]}")
      want_list=0
      continue
    fi
    case "$tok" in
      --no-default-features) default=0 ;;
      --all-features) all=1 ;;
      --features|-F) want_list=1 ;;
      --features=*)
        IFS=',' read -r -a more <<<"${tok#--features=}"
        feats+=("${more[@]}")
        ;;
    esac
  done
  local list="none"
  if (( all )); then
    list="all"
  elif (( ${#feats[@]} )); then
    list="$(for item in "${feats[@]}"; do [[ -n "$item" ]] && printf '%s\n' "${item#*/}"; done | sort -u | paste -sd, -)"
    [[ -n "$list" ]] || list="none"
  fi
  printf 'default_features=%s features=%s\n' "$default" "$list"
}

# artifact_manifest_line PATH ROLE FEATURE_RECORD — one manifest artifact record.
artifact_manifest_line() {
  local f="$1" role="$2" feat="$3"
  printf 'artifact %s sha256=%s size=%s mtime=%s role=%s %s\n' \
    "$f" "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%s' "$f")" "$(stat -c '%Y' "$f")" "$role" "$feat"
}

# artifact_write_run_manifest OUT ARCH KERNEL KERNEL_FEATURE_ARGS INITRAMFS BUILD_MANIFEST
#
# Bind a run that boots a separately built kernel. The kernel is recorded with the features it was
# built with; the initramfs record is carried over VERBATIM from the manifest of the build that
# produced it, so its hash and features are the build's own and not a claim made here. Fails if
# that manifest does not record this exact initramfs.
artifact_write_run_manifest() {
  local out="$1" arch="$2" kernel="$3" kfeat_args="$4" initramfs="$5" build_manifest="$6"
  local i_sha i_rec
  [[ -f "$kernel" && -f "$initramfs" && -f "$build_manifest" ]] || {
    echo "[error] ARTIFACT-BINDING: cannot bind run: kernel=$kernel initramfs=$initramfs manifest=$build_manifest"
    return 1
  }
  i_sha="$(sha256sum "$initramfs" | cut -d' ' -f1)"
  i_rec="$(_ab_record_for "$build_manifest" "$(basename "$initramfs")" "$i_sha" initramfs)"
  if [[ -z "$i_rec" ]]; then
    echo "[error] ARTIFACT-BINDING: $build_manifest does not record initramfs $(basename "$initramfs") sha256=${i_sha}"
    return 1
  fi
  {
    echo "commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "arch=$arch"
    echo "bound_from=$build_manifest"
    artifact_manifest_line "$kernel" kernel "$(artifact_features_record "$kfeat_args")"
    printf '%s\n' "$i_rec"
  } >"$out"
}

# _ab_record_for MANIFEST BASENAME SHA ROLE — print the one artifact record with that basename, hash
# and role (empty if there is none or more than one).
_ab_record_for() {
  local manifest="$1" base="$2" sha="$3" role="$4" line n=0 hit=""
  while IFS= read -r line; do
    [[ "$line" == artifact\ * ]] || continue
    local p rest
    p="$(printf '%s' "$line" | awk '{print $2}')"
    rest="${line#artifact "$p" }"
    [[ "$(basename "$p")" == "$base" ]] || continue
    [[ " $rest " == *" sha256=${sha} "* ]] || continue
    [[ " $rest " == *" role=${role} "* ]] || continue
    hit="$line"
    n=$((n + 1))
  done <"$manifest"
  (( n == 1 )) && printf '%s\n' "$hit"
  return 0
}

# _ab_field RECORD KEY — the value of KEY=… in a record.
_ab_field() {
  printf '%s\n' "$1" | tr ' ' '\n' | sed -n "s/^$2=//p" | head -1
}

# artifact_binding_check ARCH STRICT KERNEL INITRAMFS [ROLE:FEATURE ...]
#
# ROLE:FEATURE names a feature the run requires and the artifact that must carry it, e.g.
# `initramfs:timer-contract-witness`. Returns non-zero only for a strict run that cannot be bound.
artifact_binding_check() {
  local arch="$1" strict="$2" kernel="$3" initramfs="$4"
  shift 4
  local manifest="${ARTIFACT_MANIFEST:-$(dirname "$kernel")/artifact-manifest.txt}"
  local -a problems=()
  local k_sha="none" i_sha="none" m_arch="" m_commit="unknown" k_rec="" i_rec="" req role feat rec have
  [[ -f "$kernel" ]] && k_sha="$(sha256sum "$kernel" | cut -d' ' -f1)"
  [[ -f "$initramfs" ]] && i_sha="$(sha256sum "$initramfs" | cut -d' ' -f1)"
  if [[ ! -f "$manifest" ]]; then
    problems+=("no artifact manifest at $manifest: the kernel was not produced by an artifact build, or the run did not bind it")
  else
    m_arch="$(sed -n 's/^arch=//p' "$manifest" | head -1)"
    m_commit="$(sed -n 's/^commit=//p' "$manifest" | head -1)"
    [[ "$m_arch" == "$arch" ]] || problems+=("manifest $manifest is for arch '${m_arch:-none}', not $arch")
    k_rec="$(_ab_record_for "$manifest" "$(basename "$kernel")" "$k_sha" kernel)"
    i_rec="$(_ab_record_for "$manifest" "$(basename "$initramfs")" "$i_sha" initramfs)"
    [[ -n "$k_rec" ]] || problems+=("kernel $kernel (sha256=${k_sha}) is not the kernel $manifest records")
    [[ -n "$i_rec" ]] || problems+=("initramfs $initramfs (sha256=${i_sha}) is not the initramfs $manifest records")
  fi
  for req in "$@"; do
    role="${req%%:*}"
    feat="${req#*:}"
    case "$role" in
      kernel) rec="$k_rec" ;;
      initramfs) rec="$i_rec" ;;
      *) problems+=("unknown artifact role in requirement '$req'"); continue ;;
    esac
    [[ -n "$rec" ]] || continue # already reported: the artifact itself is unbound
    have="$(_ab_field "$rec" features)"
    if [[ -z "$have" ]]; then
      problems+=("the $role record carries no feature list (built before BL5c); this run requires '$feat'")
    elif [[ "$have" != "all" && ",${have}," != *",${feat},"* ]]; then
      problems+=("this run requires the $role to be built with '$feat'; it was built with features=${have}")
    fi
  done
  local result="ok"
  if (( ${#problems[@]} )); then
    if [[ "$strict" == "1" ]]; then result="refused"; else result="unbound"; fi
  fi
  echo "ARTIFACT_BINDING arch=${arch} strict=${strict} manifest=${manifest} commit=${m_commit} kernel=${kernel} kernel_sha256=${k_sha} kernel_features=$(_ab_field "$k_rec" features) initramfs=${initramfs} initramfs_sha256=${i_sha} initramfs_features=$(_ab_field "$i_rec" features) required=$(IFS=,; echo "${*:-none}") result=${result}"
  local p
  for p in "${problems[@]}"; do
    if [[ "$strict" == "1" ]]; then
      echo "[error] ARTIFACT-BINDING: $p"
    else
      echo "[warn] ARTIFACT-BINDING: $p"
    fi
  done
  if [[ "$result" == "refused" ]]; then
    echo "[error] ARTIFACT-BINDING: refusing to boot: this strict run cannot be bound to compatible artifacts"
    return 1
  fi
  return 0
}

# ── Self-test ────────────────────────────────────────────────────────────────────────────────────
# `bash scripts/lib/artifact-binding.sh --self-test`: every malformed binding must be refused by a
# strict run, and every well-formed one accepted.
_ab_self_test() {
  local t pass=0 fail=0 accepted=0 refused=0
  t="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$t'" RETURN
  mk_build() { # DIR FEATURE_ARGS ARCH
    mkdir -p "$1"
    printf 'kernel %s %s\n' "$1" "$2" >"$1/kernel_boot.elf"
    printf 'initramfs %s %s\n' "$1" "$2" >"$1/initramfs-core.cpio"
    {
      echo "commit=selftest"
      echo "arch=$3"
      artifact_manifest_line "$1/kernel_boot.elf" kernel "$(artifact_features_record "$2")"
      artifact_manifest_line "$1/initramfs-core.cpio" initramfs "$(artifact_features_record "$2")"
    } >"$1/artifact-manifest.txt"
  }
  expect() { # DESC WANT(accept|refuse) ARGS…
    local desc="$1" want="$2" got out
    shift 2
    if out="$(artifact_binding_check "$@" 2>&1)"; then got=accept; else got=refuse; fi
    if [[ "$got" == "$want" ]]; then
      pass=$((pass + 1))
      [[ "$want" == accept ]] && accepted=$((accepted + 1)) || refused=$((refused + 1))
    else
      fail=$((fail + 1))
      echo "[error] ARTIFACT-BINDING self-test: '$desc' expected $want, got $got"
      printf '%s\n' "$out" | sed 's/^/    /'
    fi
  }
  local W="initramfs:timer-contract-witness"
  mk_build "$t/tcw" "--no-default-features --features timer-contract-witness" x86_64
  mk_build "$t/def" "--no-default-features" x86_64
  mk_build "$t/arm" "--no-default-features" aarch64

  expect "a bound witness build, strict" accept x86_64 1 "$t/tcw/kernel_boot.elf" "$t/tcw/initramfs-core.cpio" "$W"
  expect "a bound default build, strict, nothing required" accept x86_64 1 "$t/def/kernel_boot.elf" "$t/def/initramfs-core.cpio"
  expect "a default build where the witness is required" refuse x86_64 1 "$t/def/kernel_boot.elf" "$t/def/initramfs-core.cpio" "$W"
  expect "the same default build, non-strict, nothing required" accept x86_64 0 "$t/def/kernel_boot.elf" "$t/def/initramfs-core.cpio"
  expect "the witness kernel with the default initramfs" refuse x86_64 1 "$t/tcw/kernel_boot.elf" "$t/def/initramfs-core.cpio" "$W"
  expect "the default kernel with the witness initramfs" refuse x86_64 1 "$t/def/kernel_boot.elf" "$t/tcw/initramfs-core.cpio" "$W"
  expect "a manifest for another arch" refuse x86_64 1 "$t/arm/kernel_boot.elf" "$t/arm/initramfs-core.cpio"
  ARTIFACT_MANIFEST="$t/none.txt" expect "no manifest" refuse x86_64 1 "$t/tcw/kernel_boot.elf" "$t/tcw/initramfs-core.cpio" "$W"
  ARTIFACT_MANIFEST="$t/none.txt" expect "no manifest, non-strict" accept x86_64 0 "$t/tcw/kernel_boot.elf" "$t/tcw/initramfs-core.cpio"

  cp -r "$t/tcw" "$t/swapped"
  printf 'a kernel swapped in after the build\n' >"$t/swapped/kernel_boot.elf"
  expect "a kernel replaced after its build" refuse x86_64 1 "$t/swapped/kernel_boot.elf" "$t/swapped/initramfs-core.cpio" "$W"
  expect "the replaced kernel, non-strict" accept x86_64 0 "$t/swapped/kernel_boot.elf" "$t/swapped/initramfs-core.cpio"

  cp -r "$t/tcw" "$t/dup"
  grep '^artifact .*role=kernel' "$t/tcw/artifact-manifest.txt" >>"$t/dup/artifact-manifest.txt"
  expect "a manifest recording the kernel twice" refuse x86_64 1 "$t/dup/kernel_boot.elf" "$t/dup/initramfs-core.cpio" "$W"

  cp -r "$t/tcw" "$t/legacy"
  sed -i 's/ role=[a-z]* default_features=[01] features=[^ ]*$//' "$t/legacy/artifact-manifest.txt"
  expect "a manifest from before BL5c (no roles)" refuse x86_64 1 "$t/legacy/kernel_boot.elf" "$t/legacy/initramfs-core.cpio" "$W"

  cp -r "$t/tcw" "$t/kfeat"
  expect "the witness required of the kernel record too" accept x86_64 1 "$t/kfeat/kernel_boot.elf" "$t/kfeat/initramfs-core.cpio" "kernel:timer-contract-witness" "$W"
  expect "an unknown role" refuse x86_64 1 "$t/kfeat/kernel_boot.elf" "$t/kfeat/initramfs-core.cpio" "bootloader:x"

  mkdir -p "$t/oracle"
  printf 'an oracle feature kernel\n' >"$t/oracle/kernel_boot.elf"
  artifact_write_run_manifest "$t/oracle/run.txt" x86_64 "$t/oracle/kernel_boot.elf" "--no-default-features --features ipc-reply-timeout-oracle-core" "$t/tcw/initramfs-core.cpio" "$t/tcw/artifact-manifest.txt" >/dev/null
  ARTIFACT_MANIFEST="$t/oracle/run.txt" expect "a run-bound oracle kernel with the witness initramfs" accept x86_64 1 "$t/oracle/kernel_boot.elf" "$t/tcw/initramfs-core.cpio" "$W" "kernel:ipc-reply-timeout-oracle-core"
  ARTIFACT_MANIFEST="$t/oracle/run.txt" expect "the run manifest with a different initramfs" refuse x86_64 1 "$t/oracle/kernel_boot.elf" "$t/def/initramfs-core.cpio" "$W"
  if artifact_write_run_manifest "$t/oracle/bad.txt" x86_64 "$t/oracle/kernel_boot.elf" "--no-default-features" "$t/def/initramfs-core.cpio" "$t/tcw/artifact-manifest.txt" >/dev/null; then
    fail=$((fail + 1)); echo "[error] ARTIFACT-BINDING self-test: a run manifest claimed an initramfs its build manifest does not record"
  else
    pass=$((pass + 1)); refused=$((refused + 1))
  fi

  local r
  for r in "--no-default-features|default_features=0 features=none" \
           "--no-default-features --features b,a|default_features=0 features=a,b" \
           "--features=yarm/x,y --features y|default_features=1 features=x,y" \
           "--all-features|default_features=1 features=all"; do
    if [[ "$(artifact_features_record "${r%%|*}")" == "${r#*|}" ]]; then
      pass=$((pass + 1))
    else
      fail=$((fail + 1)); echo "[error] ARTIFACT-BINDING self-test: features of '${r%%|*}' = '$(artifact_features_record "${r%%|*}")', want '${r#*|}'"
    fi
  done

  if (( fail )); then
    echo "[error] ARTIFACT-BINDING self-test FAILED (${fail} of $((pass + fail)))"
    return 1
  fi
  echo "[ok] ARTIFACT-BINDING self-test passed (${accepted} accepted, ${refused} refused, 4 feature records)"
}

if [[ "${BASH_SOURCE[0]}" == "$0" && "${1:-}" == "--self-test" ]]; then
  _ab_self_test
fi
