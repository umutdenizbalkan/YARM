// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod compositor;
pub mod display;
pub mod shell;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_scoped_ui_modules_are_include_only_shims() {
        let compositor_service = include_str!("compositor/service.rs");
        let display_service = include_str!("display/service.rs");
        let shell_service = include_str!("shell/service.rs");

        assert!(
            compositor_service.contains("/crates/yarm-ui-servers/src/ui/compositor/service.rs")
        );
        assert!(display_service.contains("/crates/yarm-ui-servers/src/ui/display/service.rs"));
        assert!(shell_service.contains("/crates/yarm-ui-servers/src/ui/shell/service.rs"));
    }
}
