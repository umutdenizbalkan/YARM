// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod fd_table;
pub mod mount_table;
pub mod path;
pub mod service;

pub use fd_table::VfsFdTable;
pub use mount_table::VfsMountTable;
pub use service::run;
