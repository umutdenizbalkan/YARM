// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub(crate) mod boot_command_line;
pub mod cap_transfer_split;
pub mod capabilities;
/// QEMU-CONTEXT1 §3 — the kernel half of the user execution-state witness (markers only).
#[cfg(any(test, feature = "context1-witness"))]
pub mod context_witness;
pub mod deadline_token;
pub mod direct_ack_census;
pub mod direct_ack_store;
pub mod direct_dispatch;
pub mod direct_disposition;
pub mod direct_eligibility;
pub mod direct_ipc_counters;
pub mod dispatch_post_work;
pub mod frame_allocator;
pub mod global_allocator;
pub mod idle_boundary;
pub mod ipc;
pub mod ipccall_direct;
pub mod ipccall_direct_txn;
pub mod lock;
// QEMU-LOCK1: the real subdomain-lock contention / interrupt-progress witness. Default off.
#[cfg(feature = "riscv64-lock1-witness")]
pub mod lock1_witness;
// QEMU-LOCK2: the AArch64 twin — the same lock, SGI work attributed at the GICv2. Default off.
#[cfg(feature = "aarch64-lock2-witness")]
pub mod lock2_witness;
// The facade `SpinLockIrq` calls for the one witnessed lock (LOCK1 or LOCK2, never both).
#[cfg(feature = "lock-witness")]
pub mod lock_witness;
#[cfg(any(
    feature = "aarch64-overtaken-witness",
    feature = "x86-overtaken-witness"
))]
pub mod overtaken_witness;
pub mod printk;
pub mod process;
pub mod recv_core;
pub mod recv_waiter_split;
pub mod scheduler;
pub mod scheduler_timer;
pub mod smp;
/// Stage 199D-WA3A — production-enforced exact task status transitions.
pub mod spawn_reservation;
pub mod syscall;
pub mod syscall_split;
pub mod task;
/// U9-SPAWN1 SP-1: THE task-enqueue policy and its rank-1 commit.
pub(crate) mod task_enqueue;
pub(crate) mod task_transition;
pub mod terminal_ownership;
pub mod time;
pub mod topology;
pub mod trap;
pub mod trapframe;
/// QEMU-CONTEXT1 §2 — per-task user FP/SIMD and control-state homes, and their pure rules.
pub mod user_fpu;
pub mod vm;

pub use boot::{Bootstrap, KernelState};
pub use yarm_ipc_abi::{driver_abi, process_abi, vfs_abi};
