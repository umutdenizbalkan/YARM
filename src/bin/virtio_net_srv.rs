// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

fn main() {
    yarm::services::drivers::virtio_net::run();
}
