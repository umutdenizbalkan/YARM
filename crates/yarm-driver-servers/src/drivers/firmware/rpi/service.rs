// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Pure Raspberry Pi / VideoCore firmware-property dispatch scaffold.
//!
//! This module has no IPC loop. It is not registered in an image manifest or
//! spawned by init, and `run` intentionally consumes no startup slots or MMIO.

use super::{RpiFirmwareClient, RpiMemoryRegion, RpiPropertyTransport};
use yarm_ipc_abi::platform::rpi::property_mailbox_abi::MailboxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpiFirmwareCommand {
    GetFirmwareRevision,
    GetBoardModel,
    GetBoardRevision,
    GetBoardSerial,
    GetArmMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpiFirmwareReply {
    FirmwareRevision(u32),
    BoardModel(u32),
    BoardRevision(u32),
    BoardSerial(u64),
    ArmMemory(RpiMemoryRegion),
}

pub fn dispatch_rpi_firmware_request<T: RpiPropertyTransport>(
    client: &mut RpiFirmwareClient<T>,
    command: RpiFirmwareCommand,
) -> Result<RpiFirmwareReply, MailboxError> {
    match command {
        RpiFirmwareCommand::GetFirmwareRevision => client
            .get_firmware_revision()
            .map(RpiFirmwareReply::FirmwareRevision),
        RpiFirmwareCommand::GetBoardModel => {
            client.get_board_model().map(RpiFirmwareReply::BoardModel)
        }
        RpiFirmwareCommand::GetBoardRevision => client
            .get_board_revision()
            .map(RpiFirmwareReply::BoardRevision),
        RpiFirmwareCommand::GetBoardSerial => {
            client.get_board_serial().map(RpiFirmwareReply::BoardSerial)
        }
        RpiFirmwareCommand::GetArmMemory => {
            client.get_arm_memory().map(RpiFirmwareReply::ArmMemory)
        }
    }
}

/// Deferred entrypoint: deliberately no IPC receive loop, startup-slot access,
/// MMIO discovery, or volatile access.
pub fn run() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::firmware::rpi::transport::{
        MockRpiPropertyTransport, MockRpiPropertyValues,
    };

    #[test]
    fn dispatch_is_pure_over_the_mock_client() {
        let transport = MockRpiPropertyTransport::new(MockRpiPropertyValues {
            board_model: Some(0x17),
            ..MockRpiPropertyValues::default()
        });
        let mut client = RpiFirmwareClient::new(transport);
        assert_eq!(
            dispatch_rpi_firmware_request(&mut client, RpiFirmwareCommand::GetBoardModel),
            Ok(RpiFirmwareReply::BoardModel(0x17))
        );
    }

    #[test]
    fn scaffold_has_no_live_ipc_or_hosted_mmio_path() {
        let service = include_str!("service.rs");
        let transport = include_str!("transport.rs");
        let startup_receive = ["recv", "_startup"].concat();
        let ipc_receive = ["ipc", "_recv"].concat();
        let volatile_read = ["read", "_volatile"].concat();
        assert!(!service.contains(&startup_receive));
        assert!(!service.contains(&ipc_receive));
        assert!(!service.contains(&volatile_read));
        assert!(!transport.contains(&volatile_read));
        assert!(transport.contains("#[cfg(not(feature = \"hosted-dev\"))]"));
        assert!(service.contains("pub fn run() {}"));
    }
}
