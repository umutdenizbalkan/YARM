// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServiceGraph {
    pub init_tid: u64,
    pub process_manager_tid: u64,
    pub vfs_tid: u64,
    pub supervisor_tid: u64,
    pub posix_compat_tid: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoreServiceHandles {
    pub init_tid: Option<u64>,
    pub process_manager_tid: Option<u64>,
    pub vfs_tid: Option<u64>,
    pub supervisor_tid: Option<u64>,
    pub posix_compat_tid: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServiceImagePlan {
    pub process_manager_entry: usize,
    pub vfs_entry: usize,
    pub supervisor_entry: usize,
    pub posix_compat_entry: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreLaunchReport {
    pub process_manager_spawned: bool,
    pub vfs_spawned: bool,
    pub supervisor_spawned: bool,
    pub posix_compat_spawned: bool,
}
