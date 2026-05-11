// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(not(feature = "hosted-dev"))]
pub use yarm_freestanding_alloc::install as install_freestanding_allocator;

pub use yarm_ipc_abi as ipc_abi;
pub use yarm_user_rt as user_rt;

#[inline]
pub fn install_startup_arg_slots(slots: [u64; 18]) {
    user_rt::runtime::install_startup_arg_slots(slots);
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_reexports_curated_runtime_surfaces() {
        let src = include_str!("lib.rs");
        assert!(
            !src.contains("pub use yarm::*;"),
            "server runtime must not glob re-export root yarm surfaces"
        );
        assert!(
            src.contains("pub use yarm_ipc_abi as ipc_abi;"),
            "server runtime must expose ipc abi curated surface"
        );
        assert!(
            src.contains("pub use yarm_user_rt as user_rt;"),
            "server runtime must expose user rt curated surface"
        );
    }
}
