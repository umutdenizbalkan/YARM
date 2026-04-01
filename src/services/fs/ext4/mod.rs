// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod dir;
pub mod file;
pub mod fs;
pub mod inode;
pub mod service;

pub use fs::Ext4Backend;
pub use service::{Ext4Service, run};
