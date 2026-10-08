// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! The default-off lock-contention witness facade: the four hooks `SpinLockIrq` calls for the ONE
//! witnessed instance (`vm_state_lock`, tagged with [`VM_LOCK_ID`]), dispatched to the building
//! architecture's witness — QEMU-LOCK1 on RISC-V, QEMU-LOCK2 on AArch64. Exactly one of the two is
//! built; `lock-witness` alone, or both together, is refused at compile time.

#[cfg(all(feature = "riscv64-lock1-witness", feature = "aarch64-lock2-witness"))]
compile_error!("riscv64-lock1-witness and aarch64-lock2-witness are mutually exclusive");
#[cfg(not(any(feature = "riscv64-lock1-witness", feature = "aarch64-lock2-witness")))]
compile_error!("lock-witness is internal: enable riscv64-lock1-witness or aarch64-lock2-witness");

#[cfg(feature = "riscv64-lock1-witness")]
pub use crate::kernel::lock1_witness::{
    VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released,
};
#[cfg(feature = "aarch64-lock2-witness")]
pub use crate::kernel::lock2_witness::{
    VM_LOCK_ID, maybe_hold, note_acquired, note_contended, note_released,
};
