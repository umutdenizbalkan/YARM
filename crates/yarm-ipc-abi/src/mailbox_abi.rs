// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Compatibility alias for the Raspberry Pi / VideoCore property-mailbox ABI.
//!
//! New code should import [`crate::platform::rpi::property_mailbox_abi`]. This
//! module remains public so existing users keep the same source path and wire
//! constants while the protocol's platform-specific ownership is explicit.

pub use crate::platform::rpi::property_mailbox_abi::*;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_path_reexports_rpi_property_protocol() {
        assert_eq!(super::PROPERTY_CHANNEL, 8);
        assert_eq!(
            super::GET_BOARD_MODEL,
            crate::platform::rpi::property_mailbox_abi::GET_BOARD_MODEL
        );
    }
}
