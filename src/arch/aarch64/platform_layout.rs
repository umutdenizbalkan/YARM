// AArch64 prototype platform layout constants.
// NOTE: values intentionally mirror current prototype behavior until
// architecture-specific bootstrap mappings are implemented.

pub const KERNEL_BOOTSTRAP_VIRT_BASE: u64 = 0xFFFF_0000;
pub const KERNEL_BOOTSTRAP_PHYS_BASE: u64 = 0x0;
pub const NEXT_ANON_PHYS_BASE: u64 = 0x1000_0000;

pub const MAX_IRQ_LINES: usize = 64;
pub const MAX_CPUS: usize = 64;

pub const BOOTSTRAP_CPU_ID: u8 = 0;
pub const BOOTSTRAP_TIMER_DEADLINE_TICKS: u64 = 10;
pub const PROFILE_IS_PLACEHOLDER: bool = true;

pub const GIC_CPU_IF_BASE: usize = 0x0801_0000;
