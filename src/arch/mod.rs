// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
pub mod boot_entry;
pub mod cpu_mapping;
#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) mod fdt;
pub mod hal;
pub mod hal_adapters;
pub mod irq_description;
pub mod irq_guard;
#[cfg(target_arch = "riscv64")]
pub mod riscv64;
#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "riscv64")]
pub use self::riscv64 as selected_isa;

#[cfg(target_arch = "x86_64")]
pub use self::x86_64 as selected_isa;

#[cfg(target_arch = "aarch64")]
pub use self::aarch64 as selected_isa;

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "x86_64",
    target_arch = "aarch64"
)))]
compile_error!("unsupported target_arch for arch::selected_isa");

pub mod platform_constants;
pub mod platform_layout;
pub mod syscall_abi;
pub mod vm_layout;

pub mod console;
pub mod topology;
pub mod trap;
pub mod trap_entry;
