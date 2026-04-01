#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Contract-doc enforcement gate:
# - ABI_CONTRACT_FREEZE.md
# - SYSCALL_ABI.md
# - PROC_VFS_CODEC_FREEZE.md
#
# Verifies required freeze markers and runs targeted frozen-contract tests.

required_docs=(
  "ABI_CONTRACT_FREEZE.md"
  "SYSCALL_ABI.md"
  "PROC_VFS_CODEC_FREEZE.md"
)

for doc in "${required_docs[@]}"; do
  if [[ ! -f "${doc}" ]]; then
    echo "[gate] missing required contract doc: ${doc}"
    exit 1
  fi
done

rg -q "src/arch/trap.rs" ABI_CONTRACT_FREEZE.md
rg -q "LinuxCompatSyscall::DISPATCH_TABLE" ABI_CONTRACT_FREEZE.md
rg -q 'SYSCALL_ABI_VERSION = 6|ABI Version: `6`' SYSCALL_ABI.md
rg -q "TransferRelease" SYSCALL_ABI.md
rg -q 'Syscall count: `5`' SYSCALL_ABI.md
rg -q "PROC_CODEC_V2_VERSION = 2" PROC_VFS_CODEC_FREEZE.md
rg -q "VFS_CODEC_V1_VERSION = 1" PROC_VFS_CODEC_FREEZE.md
rg -q "scripts/check-proc-vfs-codec-freeze.sh" PROC_VFS_CODEC_FREEZE.md

cargo test -q trap_router_maps_syscall
cargo test -q proc_v2_golden_vector_is_stable
cargo test -q vfs_v1_golden_vector_is_stable

echo "[ok] contract-doc enforcement gate passed"
