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
# The v13 change (FUTEX-ABS-BITSET1): NR 9's absolute-deadline flag and NR 9 / NR 10's bitset flag,
# their slots and MATCH_ANY, identical in the kernel and the user runtime; the change note.
if (( abi_code >= 13 )); then
  rg -q '^pub const FUTEX_FLAG_TIMED: usize = 1 << 1;$' src/kernel/boot/futex.rs
  rg -q '^pub const FUTEX_FLAG_BITSET: usize = 1 << 2;$' src/kernel/boot/futex.rs
  rg -q '^pub const FUTEX_ARG_DEADLINE: usize = 3;$' src/kernel/boot/futex.rs
  rg -q '^pub const FUTEX_ARG_MASK: usize = 4;$' src/kernel/boot/futex.rs
  rg -q '^pub const FUTEX_BITSET_MATCH_ANY: u32 = u32::MAX;$' src/kernel/boot/futex.rs
  rg -q '^    pub const FUTEX_FLAG_TIMED: usize = 1 << 1;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^    pub const FUTEX_FLAG_BITSET: usize = 1 << 2;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^    pub const FUTEX_BITSET_MATCH_ANY: u32 = u32::MAX;$' crates/yarm-user-rt/src/lib.rs
  rg -q 'v12 → v13 \(FUTEX-ABS-BITSET1\)' doc/SYSCALL_ABI.md
fi
# The v14 assignment (VFS-CORRECT1C): NR 21 IpcCallerSession in the kernel, the user runtime and
# the documented table; the change note.
if (( abi_code >= 14 )); then
  rg -q '^pub const SYSCALL_IPC_CALLER_SESSION_NR: usize = 21;$' src/kernel/syscall.rs
  rg -q '^    pub const SYSCALL_IPC_CALLER_SESSION_NR: usize = 21;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^\| `21` \| `IpcCallerSession` \| public' doc/SYSCALL_ABI.md
  rg -q 'v13 → v14 \(VFS-CORRECT1C\)' doc/SYSCALL_ABI.md
fi
# VFS-CORRECT1B (within v13, privileged extensions): NR 23 / NR 29 return unpacked full-width
# lanes and take the admin-endpoint out-address; slot 17 is kernel-owned; the change note.
rg -q 'Within v13 \(VFS-CORRECT1B\), privileged extensions only' doc/SYSCALL_ABI.md
rg -q 'committed.spawner_service_send as usize' src/kernel/syscall/process.rs
rg -q 'startup_args\[17\] = 0;' src/kernel/syscall/process.rs
rg -q '^    pub const STARTUP_SLOT_SERVICE_ADMIN_RECV_EP: usize = 17;$' crates/yarm-user-rt/src/lib.rs
rg -q '^pub const VFS_OP_MOUNT_STAGE: u16 = 29;$' crates/yarm-ipc-abi/src/vfs_abi.rs
rg -q '^pub const VFS_OP_MOUNT_COMMIT: u16 = 30;$' crates/yarm-ipc-abi/src/vfs_abi.rs
rg -q '### 3.2 Mount authority and capability identities \(VFS-CORRECT1B\)' doc/VFS.md
if rg -q 'packed_ret2' src/kernel/syscall/process.rs src/kernel/syscall/spawn_image_txn.rs src/kernel/syscall_split.rs; then
  echo "[gate] a packed spawn return lane is back"; exit 1
fi
rg -q "TransferRelease" doc/SYSCALL_ABI.md
rg -q "PROC_CODEC_V2_VERSION = 2" doc/VFS.md
rg -q "VFS_CODEC_V1_VERSION = 1" doc/VFS.md
rg -q "scripts/check-proc-vfs-codec-freeze.sh" doc/VFS.md

cargo test -q trap_router_maps_syscall
cargo test -q proc_v2_golden_vector_is_stable
cargo test -q vfs_v1_golden_vector_is_stable

echo "[ok] contract-doc enforcement gate passed"
