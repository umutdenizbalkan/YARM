#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

export RUST_MIN_STACK=${RUST_MIN_STACK:-33554432}

# Primary authoritative signal: workspace-owned network service behavior.
cargo test -q -p yarm-network-servers network::netmgr::service::tests::netmgr_tracks_link_state_events

# Primary authoritative runtime signal: workspace-owned UI runtime bin marker path.
cargo run -q -p yarm-ui-servers --bin display_srv | tee /tmp/display_srv_combined.log
rg -n "\[ui\] boot-to-shell marker" /tmp/display_srv_combined.log

# Secondary compatibility smoke: retain legacy mixed-domain recovery simulation signal.
cargo test -q services::network::sim::tests::link_flap_dhcp_rebind_and_socket_recovery_is_deterministic

echo "[ok] phase3/4 strict runtime gate passed (workspace authoritative + compatibility smoke)"
