// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod dir;
pub mod file;
pub mod fs;
pub mod inode;
pub mod service;

pub use fs::{
    EXT4_DEMO_PATH, EXT4_DEMO_PATH_PTR, EXT4_OVERSIZE_PATH, EXT4_OVERSIZE_PATH_PTR,
    EXT4_SERVICE_PATH, EXT4_SERVICE_PATH_PTR, Ext4Backend,
};
pub use service::{Ext4Service, run};
