// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Deprecated legacy namespace.
//! Workspace crates under `crates/` are the runtime dispatch entrypoints.

pub mod driver_manager;
pub mod init;
#[cfg(test)]
pub(crate) mod ipc_roundtrip;
pub mod process_manager;
pub mod supervisor;
pub mod vfs;

#[cfg(test)]
mod tests {
    #[test]
    fn migrated_control_plane_services_avoid_legacy_blocking_ipc_recv_calls() {
        let vfs_src = include_str!("vfs/service.rs");
        let supervisor_src = include_str!("supervisor/service.rs");
        let init_src = include_str!("init/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");
        let legacy_call = ["kernel", ".ipc_recv", "("].concat();

        assert!(
            !vfs_src.contains(legacy_call.as_str()),
            "vfs control-plane migration regressed to blocking ipc_recv"
        );
        assert!(
            !supervisor_src.contains(legacy_call.as_str()),
            "supervisor control-plane migration regressed to blocking ipc_recv"
        );
        assert!(
            !init_src.contains(legacy_call.as_str()),
            "init control-plane flow regressed to blocking ipc_recv"
        );
        assert!(
            !process_manager_src.contains(legacy_call.as_str()),
            "process-manager flow regressed to blocking ipc_recv"
        );
    }

    #[test]
    fn phase6_exit_gate_bundle_enforces_current_migration_invariants() {
        let mod_src = include_str!("mod.rs");
        let vfs_src = include_str!("vfs/service.rs");
        let supervisor_src = include_str!("supervisor/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");
        let roundtrip_src = include_str!("ipc_roundtrip.rs");

        assert!(
            mod_src.contains("#[cfg(test)]\npub(crate) mod ipc_roundtrip;"),
            "synthetic ipc roundtrip helper must remain test-only scoped"
        );
        assert!(
            vfs_src.contains("synthetic_roundtrip_call_reply_with_budget(")
                && roundtrip_src.contains("ipc_recv_with_deadline("),
            "vfs must retain timed receive migration via shared roundtrip helper"
        );
        assert!(
            vfs_src.contains("synthetic_roundtrip_call_reply_with_budget(")
                && roundtrip_src.contains("ipc_reply("),
            "vfs must retain reply-cap call/reply migration via shared roundtrip helper"
        );
        assert!(
            supervisor_src.contains("recv_with_budget"),
            "supervisor must retain budgeted receive migration"
        );
        assert!(
            process_manager_src.contains("synthetic_roundtrip_call_reply_with_budget(")
                && roundtrip_src.contains("ipc_recv_with_deadline("),
            "process-manager must retain timed receive migration via shared roundtrip helper"
        );
        assert!(
            process_manager_src.contains("synthetic_roundtrip_call_reply_with_budget(")
                && roundtrip_src.contains("ipc_reply("),
            "process-manager must retain reply-cap call/reply migration via shared roundtrip helper"
        );
    }

    #[test]
    fn phase4_choreography_retirement_bundle_avoids_server_send_reply_hops() {
        let vfs_src = include_str!("vfs/service.rs");
        let process_manager_src = include_str!("process_manager/service.rs");

        assert!(
            !vfs_src.contains("ipc_send(server_send_cap"),
            "vfs migrated call/reply path should not use ad-hoc server-send reply hop"
        );
        assert!(
            !process_manager_src.contains("ipc_send(server_send_cap"),
            "process-manager migrated call/reply path should not use ad-hoc server-send reply hop"
        );
    }
}
