// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Generic block service with an explicitly namespaced virtio backend.

pub mod backend;
pub mod service;

pub use backend::virtio;
pub use backend::virtio::VirtioBlkDevice;
pub use service::{BlockDeviceInfo, BlockDeviceOps, BlockWriteService};

/// Compatibility module for the former `virtio_blk::device` path.
pub mod device {
    pub use super::backend::virtio::device::*;
}

/// Compatibility alias retaining the existing service type and constructor.
pub type VirtioBlkWriteService<const SECTORS: usize> =
    BlockWriteService<backend::virtio::VirtioBlkMemoryDevice<SECTORS>>;

pub fn run() {
    service::run_with_backend(backend::virtio::VirtioBlkMemoryDevice::<
        { service::SERVICE_SECTORS },
    >::new());
}
