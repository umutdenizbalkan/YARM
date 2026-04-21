// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub fn run_devfs() {
    yarm::services::fs::devfs::run();
}

pub fn run_initramfs() {
    yarm::services::fs::initramfs::run();
}

pub fn run_ramfs() {
    yarm::services::fs::ramfs::run();
}

pub fn run_ext4() {
    yarm::services::fs::ext4::run();
}

pub fn run_fat() {
    yarm::services::fs::fat::run();
}

pub fn run_blkcache() {
    yarm::services::fs::blkcache::run();
}
