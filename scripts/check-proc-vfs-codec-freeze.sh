#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Process/VFS typed codec freeze gate

# Pass 4 (2026-06-15): the Proc/VFS codec freeze contract was consolidated
# into doc/VFS.md §6.
[[ -f doc/VFS.md ]] || { echo "[fail] missing doc/VFS.md"; exit 1; }

# Ensure frozen constants are present where expected.
rg -n "pub const PROC_CODEC_V2_VERSION: u16 = 2;" crates/yarm-ipc-abi/src/process_abi.rs >/dev/null || {
  echo "[fail] PROC codec version drift"; exit 1;
}
rg -n "pub const VFS_CODEC_V1_VERSION: u16 = 1;" crates/yarm-ipc-abi/src/vfs_abi.rs >/dev/null || {
  echo "[fail] VFS codec version drift"; exit 1;
}

# Ensure golden-vector tests exist.
rg -n "proc_v2_golden_vector_is_stable" crates/yarm-ipc-abi/src/process_abi.rs >/dev/null || {
  echo "[fail] missing proc golden-vector test"; exit 1;
}
rg -n "vfs_v1_golden_vector_is_stable" crates/yarm-ipc-abi/src/vfs_abi.rs >/dev/null || {
  echo "[fail] missing vfs golden-vector test"; exit 1;
}

cargo test -q -p yarm-ipc-abi proc_v2_golden_vector_is_stable
cargo test -q -p yarm-ipc-abi vfs_v1_golden_vector_is_stable

echo "[ok] process/vfs codec freeze gate passed"
