// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Pure mailbox request dispatch scaffold.
//!
//! This module has no IPC loop. It is not registered in an image manifest or
//! spawned by init, and `run` intentionally consumes no startup slots or MMIO.

use super::{MailboxClient, MemoryRegion, PropertyTransport};
use yarm_ipc_abi::mailbox_abi::MailboxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxCommand {
    GetFirmwareRevision,
    GetBoardModel,
    GetBoardRevision,
    GetBoardSerial,
    GetArmMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxReply {
    FirmwareRevision(u32),
    BoardModel(u32),
    BoardRevision(u32),
    BoardSerial(u64),
    ArmMemory(MemoryRegion),
}

pub fn dispatch<T: PropertyTransport>(
    client: &mut MailboxClient<T>,
    command: MailboxCommand,
) -> Result<MailboxReply, MailboxError> {
    match command {
        MailboxCommand::GetFirmwareRevision => client
            .get_firmware_revision()
            .map(MailboxReply::FirmwareRevision),
        MailboxCommand::GetBoardModel => client.get_board_model().map(MailboxReply::BoardModel),
        MailboxCommand::GetBoardRevision => {
            client.get_board_revision().map(MailboxReply::BoardRevision)
        }
        MailboxCommand::GetBoardSerial => client.get_board_serial().map(MailboxReply::BoardSerial),
        MailboxCommand::GetArmMemory => client.get_arm_memory().map(MailboxReply::ArmMemory),
    }
}

/// Deferred entrypoint: deliberately no IPC receive loop, startup-slot access,
/// MMIO discovery, or volatile access.
pub fn run() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::mailbox::transport::{MockPropertyTransport, MockPropertyValues};

    #[test]
    fn dispatch_is_pure_over_the_mock_client() {
        let transport = MockPropertyTransport::new(MockPropertyValues {
            board_model: Some(0x17),
            ..MockPropertyValues::default()
        });
        let mut client = MailboxClient::new(transport);
        assert_eq!(
            dispatch(&mut client, MailboxCommand::GetBoardModel),
            Ok(MailboxReply::BoardModel(0x17))
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
