#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# BUILD-GATES1 §2: provision and EXPLICITLY select the disassemblers the KERNEL-NOFP1 final-image
# checker supports (`scripts/check-kernel-nofp.py`, TOOL_SUPPORTED): GNU objdump 2.42 for x86_64
# and LLVM 18 llvm-objdump for AArch64 and RISC-V. Used by every CI job that runs the checker's
# fixtures (`cargo test`) or builds a kernel (the artifact builders run the checker on the image
# they publish), so fixtures and kernel checks resolve the same tools the same way.
#
#   scripts/ci/provision-nofp-tools.sh            install (apt), select, verify
#   scripts/ci/provision-nofp-tools.sh --no-install   select and verify what is installed
#
# Selection is by absolute path through YARM_OBJDUMP / YARM_LLVM_OBJDUMP, never by whatever PATH
# happens to hold; under GitHub Actions both are exported to later steps ($GITHUB_ENV) and the
# LLVM 18 bin directory is put on PATH ($GITHUB_PATH) for llvm-objcopy. The checker's own
# `--check-tools` then version-gates each tool and probes it on known encodings for every
# architecture; a missing, unsupported or misdecoding tool fails here, by name (NOFP-TOOLS).
set -euo pipefail
cd "$(dirname "$0")/../.."

OBJDUMP=${YARM_OBJDUMP:-/usr/bin/objdump}
LLVM_BIN=${YARM_LLVM_BIN:-/usr/lib/llvm-18/bin}
LLVM_OBJDUMP=${YARM_LLVM_OBJDUMP:-$LLVM_BIN/llvm-objdump}

if [[ "${1:-}" != "--no-install" ]]; then
  sudo apt-get update
  sudo apt-get install -y --no-install-recommends binutils llvm-18 ripgrep
fi

for t in "$OBJDUMP" "$LLVM_OBJDUMP"; do
  if [[ ! -x "$t" ]]; then
    echo "[error] NOFP-TOOLS: $t is not installed (need GNU binutils 2.42 objdump and LLVM 18 llvm-objdump)"
    exit 1
  fi
done

export YARM_OBJDUMP="$OBJDUMP" YARM_LLVM_OBJDUMP="$LLVM_OBJDUMP"
python3 -I scripts/check-kernel-nofp.py --check-tools

if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "YARM_OBJDUMP=$OBJDUMP"
    echo "YARM_LLVM_OBJDUMP=$LLVM_OBJDUMP"
  } >>"$GITHUB_ENV"
fi
if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "$LLVM_BIN" >>"$GITHUB_PATH"
fi
echo "[ok] NOFP-TOOLS: selected objdump=$OBJDUMP llvm-objdump=$LLVM_OBJDUMP"
