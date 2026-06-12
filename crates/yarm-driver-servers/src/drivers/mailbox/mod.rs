// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Mock-safe userspace Raspberry Pi mailbox property scaffold.
//!
//! No live server is spawned and no hosted path can access MMIO. Future hardware
//! work must supply platform-discovered, capability-granted mailbox mappings and
//! firmware-visible aligned storage before a transport can be implemented.

pub mod client;
pub mod service;
pub mod transport;

pub use client::{MailboxClient, MemoryRegion};
pub use service::{MailboxCommand, MailboxReply, dispatch};
pub use transport::PropertyTransport;
#[cfg(any(test, feature = "hosted-dev"))]
pub use transport::{MockPropertyTransport, MockPropertyValues};
