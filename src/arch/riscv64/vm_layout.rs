// RISC-V 64 prototype VM layout constants.

pub const PAGE_SIZE: usize = 4096;
pub const KERNEL_SPACE_BASE: u64 = 0x8000_0000;
pub const ASID_BITS: u8 = 16;

pub const MAX_MAPPINGS: usize = 128;
pub const MAX_ADDRESS_SPACES: usize = 16;

pub const PROFILE_IS_PLACEHOLDER: bool = false;
