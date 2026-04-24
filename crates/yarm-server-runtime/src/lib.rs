// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(not(feature = "hosted-dev"))]
yarm_freestanding_alloc::install!(2 * 1024 * 1024, "yarm-server-runtime freestanding allocator OOM");

pub use yarm_ipc_abi as ipc_abi;
pub use yarm_user_rt as user_rt;

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
