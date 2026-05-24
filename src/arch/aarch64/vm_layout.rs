// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// AArch64 prototype VM layout constants.
// NOTE: values intentionally mirror current prototype behavior until
// architecture-specific VM mappings are implemented.

pub const PAGE_SIZE: usize = 4096;
// Keep the prototype kernel image (linked at 0x4008_0000) in the kernel half
// of the software VM split. A higher split caused bootstrap kernel mappings
// at 0x4008_xxxx to be rejected as user-space addresses during init.
pub const KERNEL_SPACE_BASE: u64 = 0x4000_0000;
pub const USER_BRK_DEFAULT_BASE: usize = 0x4000_0000;
pub const ASID_BITS: u8 = 16;

// Mapping bookkeeping is per-ASID and currently tracks page mappings one entry
// at a time. A 512 KiB user stack already needs 128 x 4 KiB entries, so
// bootstrap userspace (ELF RX/RW/BSS + stack + startup mappings) requires
// headroom above 128 to avoid early Vm(Full) failures.
#[cfg(feature = "hosted-dev")]
pub const MAX_MAPPINGS: usize = 512;
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_MAPPINGS: usize = 512;

#[cfg(feature = "hosted-dev")]
pub const MAX_ADDRESS_SPACES: usize = 16;
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_ADDRESS_SPACES: usize = 32;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
