// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod boot;
pub mod capabilities;
pub mod driver_manager;
pub mod frame_allocator;
pub mod ipc;
pub mod lock;
pub mod printk;
pub mod process;
pub mod scheduler;
pub mod scheduler_timer;
pub mod smp;
pub mod supervisor_abi;
pub mod syscall;
pub mod task;
pub mod time;
pub mod topology;
pub mod trap;
pub mod trapframe;
pub mod vfs;
pub mod vm;

pub use boot::{Bootstrap, KernelState};
pub use yarm_ipc_abi::{driver_abi, process_abi, vfs_abi};

#[cfg(test)]
mod extraction_bridge_tests;
