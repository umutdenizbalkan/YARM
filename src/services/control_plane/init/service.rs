// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![allow(deprecated)]

// Legacy shim: authoritative implementation lives in workspace crate source.
#[allow(unused_imports)]
use crate::yarm_fs_servers as _;
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/crates/yarm-control-plane-servers/src/control_plane/init/service.rs"));
