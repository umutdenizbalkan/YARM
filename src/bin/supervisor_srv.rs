// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

extern crate yarm;

fn main() {
    yarm::services::control_plane::supervisor::run();
}
