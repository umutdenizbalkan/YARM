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

#[cfg(feature = "hosted-dev")]
pub const MAX_MAPPINGS: usize = 128;
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_MAPPINGS: usize = 128;

#[cfg(feature = "hosted-dev")]
pub const MAX_ADDRESS_SPACES: usize = 16;
// Stage 163D: raised 32 -> 48 (mirrors x86_64). The COW fork clone allocates a new
// user address space; the expanded service set left no free ASID slot (or derived
// page-table-page / ASID-root budget) under the old 32-slot bound, so fork failed
// with `Vm(Full)`. 48 gives headroom for the current services plus a forked child.
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_ADDRESS_SPACES: usize = 48;

pub const PROFILE_IS_PLACEHOLDER: bool = true;
