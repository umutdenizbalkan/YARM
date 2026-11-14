#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Contract-doc enforcement gate:
# - doc/SYSCALL_ABI.md (after Pass 6 — absorbed the mechanism-layer contract freeze,
#   formerly doc/ABI_CONTRACT_FREEZE.md, which was deleted in 3c86f362 without
#   updating this gate; the gate could not pass while it grepped a missing file)
# - doc/VFS.md (after Pass 4 — consolidated from PROC_VFS_CODEC_FREEZE.md)
#
# Verifies required freeze markers and runs targeted frozen-contract tests.

required_docs=(
  "doc/SYSCALL_ABI.md"
  "doc/VFS.md"
)

for doc in "${required_docs[@]}"; do
  if [[ ! -f "${doc}" ]]; then
    echo "[gate] missing required contract doc: ${doc}"
    exit 1
  fi
done

# Mechanism-layer freeze markers (doc/SYSCALL_ABI.md "Mechanism-layer contract freeze").
rg -q "src/arch/trap.rs" doc/SYSCALL_ABI.md
rg -q "src/kernel/trapframe.rs" doc/SYSCALL_ABI.md
rg -q "src/kernel/scheduler_timer.rs" doc/SYSCALL_ABI.md
rg -q "Mechanism-layer contract freeze" doc/SYSCALL_ABI.md
# The Linux-compat dispatch-table freeze is RETIRED: `LinuxCompatSyscall::DISPATCH_TABLE`
# and `src/linux_compat/mod.rs` no longer exist in the tree. Assert the retirement is
# documented rather than re-asserting a symbol that is gone.
rg -q "Retired freeze — Linux-compat syscall dispatch table" doc/SYSCALL_ABI.md
# ABI version: cross-validated, not a hard-coded marker. The kernel's `SYSCALL_ABI_VERSION` is the
# authority; doc/SYSCALL_ABI.md must declare that same version exactly once. (This line used to
# grep for a literal `ABI Version: \`10\``, which failed from v11 — FUTEX-REQUEUE1 — onward.)
abi_code=$(sed -n 's/^pub const SYSCALL_ABI_VERSION: u16 = \([0-9][0-9]*\);$/\1/p' src/kernel/syscall.rs)
if [[ -z "${abi_code}" ]]; then
  echo "[gate] SYSCALL_ABI_VERSION not found in src/kernel/syscall.rs"
  exit 1
fi
abi_doc_lines=$(grep -c '^- ABI Version: `[0-9][0-9]*`$' doc/SYSCALL_ABI.md || true)
abi_doc=$(sed -n 's/^- ABI Version: `\([0-9][0-9]*\)`$/\1/p' doc/SYSCALL_ABI.md)
if [[ "${abi_doc_lines}" != "1" || "${abi_doc}" != "${abi_code}" ]]; then
  echo "[gate] ABI version mismatch: code=${abi_code} doc=${abi_doc:-none} (declarations: ${abi_doc_lines})"
  exit 1
fi
# The v11 assignment: NR 17 is FutexCmpRequeue in the kernel, in the user runtime and in the
# documented syscall table, and the doc records the v10 -> v11 change that assigned it.
if (( abi_code >= 11 )); then
  rg -q '^pub const SYSCALL_FUTEX_CMP_REQUEUE_NR: usize = 17;$' src/kernel/syscall.rs
  rg -q '^    pub const SYSCALL_FUTEX_CMP_REQUEUE_NR: usize = 17;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^\| `17` \| `FutexCmpRequeue` \| public' doc/SYSCALL_ABI.md
  rg -q 'v10 → v11 \(FUTEX-REQUEUE1\)' doc/SYSCALL_ABI.md
fi
# The v12 assignment (SCHED-TIME-FOUNDATION1): NR 18 ClockGetTime / NR 19 ClockSetRealtime / NR 20 ClockNanosleep in
# the kernel, the user runtime and the documented table; error code 11 Unavailable; the change note.
if (( abi_code >= 12 )); then
  rg -q '^pub const SYSCALL_CLOCK_GETTIME_NR: usize = 18;$' src/kernel/syscall.rs
  rg -q '^pub const SYSCALL_CLOCK_SET_REALTIME_NR: usize = 19;$' src/kernel/syscall.rs
  rg -q '^    pub const SYSCALL_CLOCK_GETTIME_NR: usize = 18;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^    pub const SYSCALL_CLOCK_SET_REALTIME_NR: usize = 19;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^\| `18` \| `ClockGetTime` \| public' doc/SYSCALL_ABI.md
  rg -q '^\| `19` \| `ClockSetRealtime` \| public' doc/SYSCALL_ABI.md
  rg -q '^pub const SYSCALL_CLOCK_NANOSLEEP_NR: usize = 20;$' src/kernel/syscall.rs
  rg -q '^    pub const SYSCALL_CLOCK_NANOSLEEP_NR: usize = 20;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^\| `20` \| `ClockNanosleep` \| public' doc/SYSCALL_ABI.md
  rg -q '^- `11`: `Unavailable`' doc/SYSCALL_ABI.md
  rg -q 'v11 → v12 \(SCHED-TIME-FOUNDATION1\)' doc/SYSCALL_ABI.md
fi
rg -q "TransferRelease" doc/SYSCALL_ABI.md
rg -q "PROC_CODEC_V2_VERSION = 2" doc/VFS.md
rg -q "VFS_CODEC_V1_VERSION = 1" doc/VFS.md
rg -q "scripts/check-proc-vfs-codec-freeze.sh" doc/VFS.md

cargo test -q trap_router_maps_syscall
cargo test -q proc_v2_golden_vector_is_stable
cargo test -q vfs_v1_golden_vector_is_stable

echo "[ok] contract-doc enforcement gate passed"
