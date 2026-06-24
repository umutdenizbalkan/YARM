// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

// x86_64 virtual memory layout constants.
// Split user/kernel virtual spaces at the canonical higher-half boundary.

pub const PAGE_SIZE: usize = 4096;
pub const KERNEL_SPACE_BASE: u64 = 0xFFFF_8000_0000_0000;
pub const USER_BRK_DEFAULT_BASE: usize = 0x4000_0000;
pub const ASID_BITS: u8 = 16;

#[cfg(feature = "hosted-dev")]
pub const MAX_MAPPINGS: usize = 128;
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_MAPPINGS: usize = 128;

#[cfg(feature = "hosted-dev")]
pub const MAX_ADDRESS_SPACES: usize = 16;
// Stage 163E: reverted the Stage 163D 32 -> 48 bump. Diagnostics proved the address
// space table was NOT the binding structure (asid_used=11/48, asid_retired=0): the
// fork `Vm(Full)` was the COW clone ballooning the *parent mapping table* by
// per-page splits, since fixed (see clone_user_address_space_cow). 32 is the
// well-tested value and leaves ample ASID headroom for the current service set.
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_ADDRESS_SPACES: usize = 32;

pub const PROFILE_IS_PLACEHOLDER: bool = false;
