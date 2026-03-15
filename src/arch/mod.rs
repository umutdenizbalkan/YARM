pub mod aarch64;
pub mod hal;
pub mod riscv;
pub mod riscv64;
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
pub use self::riscv64 as selected_isa;

pub mod platform_layout;
pub mod syscall_abi;
pub mod vm_layout;
