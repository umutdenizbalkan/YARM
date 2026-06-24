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
// Stage 163D: raised 32 -> 48. The expanded driver_manager service set pushed the
// live user address-space count to/over the old 32-slot budget, so the proof fork's
// `clone_user_address_space_cow` failed at `create_user_space` with `Vm(Full)` (no
// free ASID slot) — and the derived page-table-page / ASID-root pools
// (MAX_PT_PAGES, MAX_ASID_ROOTS, both = f(MAX_ADDRESS_SPACES)) scale with this knob
// too, so one bump relieves whichever of the three is binding. 48 leaves headroom
// for the current services plus a forked child. On bare-metal `PageTablePage` is
// just `{ phys: u64 }`, so the static cost of the larger pools is modest (~190 KiB).
#[cfg(not(feature = "hosted-dev"))]
pub const MAX_ADDRESS_SPACES: usize = 48;

pub const PROFILE_IS_PLACEHOLDER: bool = false;
