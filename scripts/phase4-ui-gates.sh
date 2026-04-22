#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

export RUST_MIN_STACK=${RUST_MIN_STACK:-33554432}

# Preserve the original syscall determinism precondition covered by phase4.
cargo test -q kernel::syscall::tests::transfer_send_without_waiter_returns_would_block

# Primary authoritative signal: exercise UI tests from workspace-owned crate paths.
cargo test -q -p yarm-ui-servers ui::display::service::tests::boot_marker_is_stable
cargo test -q -p yarm-ui-servers ui::display::service::tests::display_tracks_modeset_and_present
cargo test -q -p yarm-ui-servers ui::compositor::service::tests::compositor_replay_is_deterministic
cargo test -q -p yarm-ui-servers ui::shell::service::tests::shell_session_counter_increments

# Secondary compatibility smoke: ensure legacy ui namespace remains shim-only.
cargo test -q -p yarm services::ui::tests::legacy_scoped_ui_modules_are_include_only_shims

echo "[ok] phase4 ui gates passed (workspace authoritative + compatibility smoke)"
