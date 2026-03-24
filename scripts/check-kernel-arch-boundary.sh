#!/usr/bin/env bash
set -euo pipefail

fail=0

check_absent() {
  local pattern=$1
  local file=$2
  if rg -n "$pattern" "$file" >/dev/null; then
    echo "[error] architecture boundary violation pattern '$pattern' in $file"
    rg -n "$pattern" "$file"
    fail=1
  fi
}

check_absent '^pub const IPC_REGISTER_WORDS: usize = [0-9]+' src/kernel/ipc.rs
check_absent '^pub const MAX_CPUS: usize = [0-9]+' src/kernel/scheduler.rs
check_absent '^const MAX_IRQ_LINES: usize = [0-9]+' src/kernel/boot/mod.rs
check_absent 'pub args: \[usize; 6\]' src/kernel/trapframe.rs
check_absent 'SYSCALL_ARG_TRANSFER_CAP: usize = 5' src/kernel/syscall.rs
check_absent 'VirtAddr\(0xFFFF_0000\)' src/kernel/boot/mod.rs
check_absent 'next_anon_phys: 0x1000_0000' src/kernel/boot/mod.rs

# Bin-layer ISA leakage checks: boot bin must route ISA details through src/arch/*.
check_absent 'global_asm!' src/bin/kernel_boot.rs
check_absent 'kernel_entry_x86_64' src/bin/kernel_boot.rs
check_absent 'target_arch = "x86_64"' src/bin/kernel_boot.rs
check_absent 'target_arch = "riscv64"' src/bin/kernel_boot.rs
check_absent 'target_arch = "aarch64"' src/bin/kernel_boot.rs

if [[ "$fail" -ne 0 ]]; then
  echo "[error] kernel/arch boundary checks failed"
  exit 1
fi

echo "[ok] kernel/arch boundary checks passed"
