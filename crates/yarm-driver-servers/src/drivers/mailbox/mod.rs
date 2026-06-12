// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Compatibility aliases for the Raspberry Pi / VideoCore firmware property
//! mailbox scaffold. New code should use [`crate::drivers::firmware::rpi`].

#[cfg(any(test, feature = "hosted-dev"))]
pub use crate::drivers::firmware::rpi::{
    MockRpiPropertyTransport as MockPropertyTransport, MockRpiPropertyValues as MockPropertyValues,
};
pub use crate::drivers::firmware::rpi::{
    RpiFirmwareClient as MailboxClient, RpiFirmwareCommand as MailboxCommand,
    RpiFirmwareReply as MailboxReply, RpiMemoryRegion as MemoryRegion,
    RpiPropertyTransport as PropertyTransport, dispatch_rpi_firmware_request as dispatch,
};

/// Compatibility module for the former `mailbox::client` path.
pub mod client {
    pub use crate::drivers::firmware::rpi::client::{
        RpiFirmwareClient as MailboxClient, RpiMemoryRegion as MemoryRegion,
    };
}

/// Compatibility module for the former `mailbox::service` path.
pub mod service {
    pub use crate::drivers::firmware::rpi::service::{
        RpiFirmwareCommand as MailboxCommand, RpiFirmwareReply as MailboxReply,
        dispatch_rpi_firmware_request as dispatch, run,
    };
}

/// Compatibility module for the former `mailbox::transport` path.
pub mod transport {
    pub use crate::drivers::firmware::rpi::transport::RpiPropertyTransport as PropertyTransport;
    #[cfg(not(feature = "hosted-dev"))]
    pub use crate::drivers::firmware::rpi::transport::{
        DeferredRpiMailboxTransport as DeferredMmioTransport,
        GrantedRpiMailboxMapping as GrantedMailboxMapping,
    };
    #[cfg(any(test, feature = "hosted-dev"))]
    pub use crate::drivers::firmware::rpi::transport::{
        MockRpiPropertyTransport as MockPropertyTransport,
        MockRpiPropertyValues as MockPropertyValues,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_resolve_to_rpi_firmware_scaffold() {
        let _ = core::mem::size_of::<MemoryRegion>();
        let _ = core::mem::size_of::<MailboxCommand>();
        let values = MockPropertyValues::default();
        let _client = MailboxClient::new(MockPropertyTransport::new(values));
    }
}
