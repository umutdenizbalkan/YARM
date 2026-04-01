#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Process/VFS typed codec freeze gate

[[ -f PROC_VFS_CODEC_FREEZE.md ]] || { echo "[fail] missing PROC_VFS_CODEC_FREEZE.md"; exit 1; }

# Ensure frozen constants are present where expected.
rg -n "pub const PROC_CODEC_V2_VERSION: u16 = 2;" src/kernel/process_abi.rs >/dev/null || {
  echo "[fail] PROC codec version drift"; exit 1;
}
rg -n "pub const VFS_CODEC_V1_VERSION: u16 = 1;" src/kernel/vfs_abi.rs >/dev/null || {
  echo "[fail] VFS codec version drift"; exit 1;
}

# Ensure golden-vector tests exist.
rg -n "proc_v2_golden_vector_is_stable" src/kernel/process_abi.rs >/dev/null || {
  echo "[fail] missing proc golden-vector test"; exit 1;
}
rg -n "vfs_v1_golden_vector_is_stable" src/kernel/vfs_abi.rs >/dev/null || {
  echo "[fail] missing vfs golden-vector test"; exit 1;
}

cargo test -q proc_v2_golden_vector_is_stable
cargo test -q vfs_v1_golden_vector_is_stable

echo "[ok] process/vfs codec freeze gate passed"
