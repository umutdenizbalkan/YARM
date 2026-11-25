// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod mount_table;
pub mod path;
pub(crate) mod router;
pub mod service;

pub use mount_table::VfsMountTable;
pub use service::run;
