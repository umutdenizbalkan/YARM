// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod ui;
pub use ui::{compositor, display, shell};

pub fn run_compositor() {
    ui::compositor::run();
}

pub fn run_display() {
    ui::display::run();
}

pub fn run_shell() {
    ui::shell::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_ui_impl_does_not_delegate_back_to_legacy_ui_namespace() {
        let compositor_src = include_str!("ui/compositor/service.rs");
        let display_src = include_str!("ui/display/service.rs");
        let shell_src = include_str!("ui/shell/service.rs");
        let legacy_ui = ["yarm", "::services::", "ui::"].concat();

        for src in [compositor_src, display_src, shell_src] {
            assert!(
                !src.contains(legacy_ui.as_str()),
                "workspace scoped ui impl must not delegate to legacy ui namespace"
            );
        }
    }

    #[test]
    fn ui_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
            (
                "compositor_srv",
                "name = \"compositor_srv\"",
                "path = \"src/bin/compositor_srv.rs\"",
                "bin/compositor_srv.rs",
                "run_compositor",
            ),
            (
                "display_srv",
                "name = \"display_srv\"",
                "path = \"src/bin/display_srv.rs\"",
                "bin/display_srv.rs",
                "run_display",
            ),
            (
                "shell_srv",
                "name = \"shell_srv\"",
                "path = \"src/bin/shell_srv.rs\"",
                "bin/shell_srv.rs",
                "run_shell",
            ),
        ];

        for (bin_name, name_entry, path_entry, bin_path, run_fn) in expected_bins {
            assert!(
                cargo_toml.contains(name_entry),
                "Cargo.toml missing expected bin entry: {bin_name}"
            );
            assert!(
                cargo_toml.contains(path_entry),
                "Cargo.toml missing expected bin path for: {bin_name}"
            );

            let src = match bin_path {
                "bin/compositor_srv.rs" => include_str!("bin/compositor_srv.rs"),
                "bin/display_srv.rs" => include_str!("bin/display_srv.rs"),
                "bin/shell_srv.rs" => include_str!("bin/shell_srv.rs"),
                _ => panic!("unexpected bin path in parity table: {bin_path}"),
            };
            assert!(
                src.contains("yarm_ui_servers::"),
                "{bin_name} should dispatch via yarm_ui_servers crate entrypoint"
            );
            assert!(
                src.contains(run_fn),
                "{bin_name} should call {run_fn} for parity with ui service mapping"
            );
        }
    }
}
