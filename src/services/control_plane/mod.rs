// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Deprecated legacy namespace.
//! Workspace crates under `crates/` are the runtime dispatch entrypoints.

pub mod init;
pub(crate) mod ipc_roundtrip;
pub mod process_manager;
pub mod supervisor;
pub mod vfs;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_control_plane_modules_are_include_only_shims() {
        let init_src = include_str!("init/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");
        let supervisor_src = include_str!("supervisor/service.rs");
        let vfs_src = include_str!("vfs/service.rs");
        let roundtrip_src = include_str!("ipc_roundtrip.rs");

        assert!(init_src.contains("/crates/yarm-control-plane-servers/src/control_plane/init/service.rs"));
        assert!(process_manager_src.contains("/crates/yarm-control-plane-servers/src/control_plane/process_manager/service.rs"));
        assert!(supervisor_src.contains("/crates/yarm-control-plane-servers/src/control_plane/supervisor/service.rs"));
        assert!(vfs_src.contains("/crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs"));
        assert!(roundtrip_src.contains("/crates/yarm-control-plane-servers/src/control_plane/ipc_roundtrip.rs"));
    }
}
