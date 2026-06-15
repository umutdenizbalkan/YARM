#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Contract-doc enforcement gate:
# - doc/ABI_CONTRACT_FREEZE.md
# - doc/SYSCALL_ABI.md
# - doc/VFS.md (after Pass 4 — consolidated from PROC_VFS_CODEC_FREEZE.md)
#
# Verifies required freeze markers and runs targeted frozen-contract tests.

required_docs=(
  "doc/ABI_CONTRACT_FREEZE.md"
  "doc/SYSCALL_ABI.md"
  "doc/VFS.md"
)

for doc in "${required_docs[@]}"; do
  if [[ ! -f "${doc}" ]]; then
    echo "[gate] missing required contract doc: ${doc}"
    exit 1
  fi
done

rg -q "src/arch/trap.rs" doc/ABI_CONTRACT_FREEZE.md
rg -q "LinuxCompatSyscall::DISPATCH_TABLE" doc/ABI_CONTRACT_FREEZE.md
rg -q 'ABI Version: `10`' doc/SYSCALL_ABI.md
rg -q "TransferRelease" doc/SYSCALL_ABI.md
rg -q "PROC_CODEC_V2_VERSION = 2" doc/VFS.md
rg -q "VFS_CODEC_V1_VERSION = 1" doc/VFS.md
rg -q "scripts/check-proc-vfs-codec-freeze.sh" doc/VFS.md

cargo test -q trap_router_maps_syscall
cargo test -q proc_v2_golden_vector_is_stable
cargo test -q vfs_v1_golden_vector_is_stable

echo "[ok] contract-doc enforcement gate passed"
