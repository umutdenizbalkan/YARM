#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

# Current safe gate for active contracts.
#
# We intentionally do NOT use:
#   cargo test --workspace --all-targets --no-run
# as a global gate yet, because it force-enables lib-test compilation for crates
# that still contain compile-dead legacy #[cfg(test)] code tied to removed
# kernel/std-era APIs.
#
# Coverage strategy for now:
# - Production/build surfaces: cargo check --workspace
# - Current shared contract suites: yarm-ipc-abi + yarm-srv-common
#
# Control-plane/root legacy unit-test paths must be rebuilt module-by-module
# around current transport/runtime seams before all-targets can be restored.

cargo check --workspace
cargo test -p yarm-ipc-abi
cargo test -p yarm-srv-common

# Optional current-only suites can be enabled explicitly when needed:
# cargo test -p yarm-user-rt
# cargo test -p yarm-control-plane-servers --lib   # NOT supported yet
