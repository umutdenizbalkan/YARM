// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(
    2 * 1024 * 1024,
    "driver manager freestanding allocator OOM"
);

fn main() {
    yarm_control_plane_servers::run_driver_manager_demo();
}
