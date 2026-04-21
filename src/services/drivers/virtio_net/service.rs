// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![allow(deprecated)]

// Legacy shim: authoritative implementation lives in workspace crate source.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/yarm-driver-servers/src/drivers/virtio_net/service.rs"
));
