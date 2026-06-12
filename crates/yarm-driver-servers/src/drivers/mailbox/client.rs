// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Typed property client helpers backed by an injected transport.

use super::transport::PropertyTransport;
use yarm_ipc_abi::mailbox_abi::{
    AlignedPropertyBuffer, GET_ARM_MEMORY, GET_BOARD_MODEL, GET_BOARD_REVISION, GET_BOARD_SERIAL,
    GET_FIRMWARE_REVISION, MailboxError, PropertyRequestEncoder, PropertyResponse,
};

const CLIENT_BUFFER_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u32,
    pub size: u32,
}

pub struct MailboxClient<T> {
    transport: T,
}

impl<T: PropertyTransport> MailboxClient<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn get_firmware_revision(&mut self) -> Result<u32, MailboxError> {
        self.query_u32(GET_FIRMWARE_REVISION)
    }

    pub fn get_board_model(&mut self) -> Result<u32, MailboxError> {
        self.query_u32(GET_BOARD_MODEL)
    }

    pub fn get_board_revision(&mut self) -> Result<u32, MailboxError> {
        self.query_u32(GET_BOARD_REVISION)
    }

    pub fn get_board_serial(&mut self) -> Result<u64, MailboxError> {
        let mut buffer = AlignedPropertyBuffer::<CLIENT_BUFFER_LEN>::zeroed();
        let used = encode_query(buffer.as_mut_slice(), GET_BOARD_SERIAL, 8)?;
        self.transport
            .transact(&mut buffer.as_mut_slice()[..used])?;
        let response = PropertyResponse::parse(&buffer.as_slice()[..used])?;
        decode_u64(response.tag(GET_BOARD_SERIAL)?.value)
    }

    pub fn get_arm_memory(&mut self) -> Result<MemoryRegion, MailboxError> {
        let mut buffer = AlignedPropertyBuffer::<CLIENT_BUFFER_LEN>::zeroed();
        let used = encode_query(buffer.as_mut_slice(), GET_ARM_MEMORY, 8)?;
        self.transport
            .transact(&mut buffer.as_mut_slice()[..used])?;
        let response = PropertyResponse::parse(&buffer.as_slice()[..used])?;
        let value = response.tag(GET_ARM_MEMORY)?.value;
        if value.len() != 8 {
            return Err(MailboxError::Malformed);
        }
        Ok(MemoryRegion {
            base: decode_u32(&value[..4])?,
            size: decode_u32(&value[4..])?,
        })
    }

    fn query_u32(&mut self, tag: u32) -> Result<u32, MailboxError> {
        let mut buffer = AlignedPropertyBuffer::<CLIENT_BUFFER_LEN>::zeroed();
        let used = encode_query(buffer.as_mut_slice(), tag, 4)?;
        self.transport
            .transact(&mut buffer.as_mut_slice()[..used])?;
        let response = PropertyResponse::parse(&buffer.as_slice()[..used])?;
        decode_u32(response.tag(tag)?.value)
    }
}

fn encode_query(buffer: &mut [u8], tag: u32, value_size: usize) -> Result<usize, MailboxError> {
    let mut encoder = PropertyRequestEncoder::new(buffer)?;
    encoder.push_tag(tag, &[], value_size)?;
    encoder.finish()
}

fn decode_u32(value: &[u8]) -> Result<u32, MailboxError> {
    if value.len() != 4 {
        return Err(MailboxError::Malformed);
    }
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn decode_u64(value: &[u8]) -> Result<u64, MailboxError> {
    if value.len() != 8 {
        return Err(MailboxError::Malformed);
    }
    Ok(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::mailbox::transport::{MockPropertyTransport, MockPropertyValues};

    #[test]
    fn board_serial_helper_returns_typed_mock_value() {
        let transport = MockPropertyTransport::new(MockPropertyValues {
            board_serial: Some(0x0123_4567_89ab_cdef),
            ..MockPropertyValues::default()
        });
        let mut client = MailboxClient::new(transport);
        assert_eq!(client.get_board_serial(), Ok(0x0123_4567_89ab_cdef));
        assert_eq!(client.transport().transactions(), 1);
    }

    #[test]
    fn arm_memory_helper_returns_typed_mock_region() {
        let transport = MockPropertyTransport::new(MockPropertyValues {
            arm_memory: Some((0, 0x8000_0000)),
            ..MockPropertyValues::default()
        });
        let mut client = MailboxClient::new(transport);
        assert_eq!(
            client.get_arm_memory(),
            Ok(MemoryRegion {
                base: 0,
                size: 0x8000_0000,
            })
        );
    }

    #[test]
    fn scalar_helpers_return_typed_mock_values() {
        let transport = MockPropertyTransport::new(MockPropertyValues {
            firmware_revision: Some(0x1234_5678),
            board_model: Some(0x17),
            board_revision: Some(0x00a0_2082),
            ..MockPropertyValues::default()
        });
        let mut client = MailboxClient::new(transport);
        assert_eq!(client.get_firmware_revision(), Ok(0x1234_5678));
        assert_eq!(client.get_board_model(), Ok(0x17));
        assert_eq!(client.get_board_revision(), Ok(0x00a0_2082));
    }
}
