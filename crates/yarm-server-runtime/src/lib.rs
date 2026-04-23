// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(not(feature = "hosted-dev"))]
yarm_freestanding_alloc::install!(2 * 1024 * 1024, "yarm-server-runtime freestanding allocator OOM");

pub use yarm::*;

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_reexports_root_yarm_surfaces() {
        let src = include_str!("lib.rs");
        assert!(
            src.contains("pub use yarm::*;"),
            "server runtime must re-export root yarm surfaces"
        );
    }
}
