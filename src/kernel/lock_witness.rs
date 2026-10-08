// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! The default-off lock-contention witness facade: the four hooks `SpinLockIrq` calls for the ONE
//! witnessed instance (`vm_state_lock`, tagged with [`VM_LOCK_ID`]), dispatched to the building
//! architecture's witness — QEMU-LOCK1 on RISC-V, QEMU-LOCK2 on AArch64, QEMU-LOCK3 on x86_64.
//! Exactly one is built; `lock-witness` alone, or any two together, is refused at compile time.

#[cfg(all(feature = "riscv64-lock1-witness", feature = "aarch64-lock2-witness"))]
compile_error!("riscv64-lock1-witness and aarch64-lock2-witness are mutually exclusive");
#[cfg(all(feature = "riscv64-lock1-witness", feature = "x86_64-lock3-witness"))]
compile_error!("riscv64-lock1-witness and x86_64-lock3-witness are mutually exclusive");
#[cfg(all(feature = "aarch64-lock2-witness", feature = "x86_64-lock3-witness"))]
compile_error!("aarch64-lock2-witness and x86_64-lock3-witness are mutually exclusive");
#[cfg(not(any(
    feature = "riscv64-lock1-witness",
    feature = "aarch64-lock2-witness",
    feature = "x86_64-lock3-witness"
)))]
compile_error!("lock-witness is internal: enable one of the LOCK1, LOCK2 or LOCK3 witnesses");

#[cfg(feature = "riscv64-lock1-witness")]
pub use crate::kernel::lock1_witness::{
    VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released,
};
#[cfg(feature = "aarch64-lock2-witness")]
pub use crate::kernel::lock2_witness::{
    VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released,
};
#[cfg(feature = "x86_64-lock3-witness")]
pub use crate::kernel::lock3_witness::{
    VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_instance, note_released,
};

/// The acquiring instance's address, just before its contention or acquisition record. LOCK1 and
/// LOCK2 identify their one instance by id alone and record nothing here.
#[cfg(any(feature = "riscv64-lock1-witness", feature = "aarch64-lock2-witness"))]
#[inline(always)]
pub fn note_instance(_id: u32, _instance: usize) {}
