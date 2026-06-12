// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Raspberry Pi / VideoCore firmware property-mailbox scaffold.
//!
//! This is not a generic mailbox driver. No live server is spawned and no
//! hosted path can access MMIO. Real transport remains deferred.

pub mod client;
pub mod service;
pub mod transport;

pub use client::{RpiFirmwareClient, RpiMemoryRegion};
pub use service::{RpiFirmwareCommand, RpiFirmwareReply, dispatch_rpi_firmware_request};
pub use transport::RpiPropertyTransport;
#[cfg(any(test, feature = "hosted-dev"))]
pub use transport::{MockRpiPropertyTransport, MockRpiPropertyValues};
