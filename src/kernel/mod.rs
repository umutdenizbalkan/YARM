// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub(crate) mod boot_command_line;
pub mod cap_transfer_split;
pub mod capabilities;
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
pub mod ipc;
pub mod ipccall_direct;
pub mod ipccall_direct_txn;
pub mod lock;
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
pub mod vm;

pub use boot::{Bootstrap, KernelState};
pub use yarm_ipc_abi::{driver_abi, process_abi, vfs_abi};
