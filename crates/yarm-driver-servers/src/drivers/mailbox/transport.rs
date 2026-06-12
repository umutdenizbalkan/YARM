// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Property transport boundary and hosted mock.

use yarm_ipc_abi::mailbox_abi::MailboxError;

pub trait PropertyTransport {
    /// Firmware-style in-place transaction: a successful transport overwrites
    /// the request buffer with a response buffer.
    fn transact(&mut self, buffer: &mut [u8]) -> Result<(), MailboxError>;
}

/// Values returned by the deterministic hosted property transport.
#[cfg(any(test, feature = "hosted-dev"))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MockPropertyValues {
    pub firmware_revision: Option<u32>,
    pub board_model: Option<u32>,
    pub board_revision: Option<u32>,
    pub board_serial: Option<u64>,
    pub arm_memory: Option<(u32, u32)>,
    pub vc_memory: Option<(u32, u32)>,
}

/// Hosted-only backend that validates a request and overwrites it in place.
#[cfg(any(test, feature = "hosted-dev"))]
pub struct MockPropertyTransport {
    values: MockPropertyValues,
    transactions: usize,
}

#[cfg(any(test, feature = "hosted-dev"))]
impl MockPropertyTransport {
    pub const fn new(values: MockPropertyValues) -> Self {
        Self {
            values,
            transactions: 0,
        }
    }

    pub const fn transactions(&self) -> usize {
        self.transactions
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
impl PropertyTransport for MockPropertyTransport {
    fn transact(&mut self, buffer: &mut [u8]) -> Result<(), MailboxError> {
        use yarm_ipc_abi::mailbox_abi::{
            END_TAG, GET_ARM_MEMORY, GET_BOARD_MODEL, GET_BOARD_REVISION, GET_BOARD_SERIAL,
            GET_FIRMWARE_REVISION, GET_VC_MEMORY, PropertyRequest, RESPONSE_SUCCESS,
            TAG_RESPONSE_BIT,
        };

        let size = read_u32(buffer, 0)? as usize;
        PropertyRequest::parse(buffer.get(..size).ok_or(MailboxError::Malformed)?)?;

        let mut cursor = 8usize;
        loop {
            let tag = read_u32(buffer, cursor)?;
            if tag == END_TAG {
                break;
            }
            let value_capacity = read_u32(buffer, cursor + 4)? as usize;
            let padded = value_capacity
                .checked_add(3)
                .map(|value| value & !3)
                .ok_or(MailboxError::Malformed)?;
            let value_start = cursor.checked_add(12).ok_or(MailboxError::Malformed)?;
            let next = value_start
                .checked_add(padded)
                .ok_or(MailboxError::Malformed)?;
            if next.checked_add(4).ok_or(MailboxError::Malformed)? > size {
                return Err(MailboxError::Malformed);
            }

            let mut encoded = [0u8; 8];
            let response: &[u8] = match tag {
                GET_FIRMWARE_REVISION => encode_u32(self.values.firmware_revision, &mut encoded)?,
                GET_BOARD_MODEL => encode_u32(self.values.board_model, &mut encoded)?,
                GET_BOARD_REVISION => encode_u32(self.values.board_revision, &mut encoded)?,
                GET_BOARD_SERIAL => encode_u64(self.values.board_serial, &mut encoded)?,
                GET_ARM_MEMORY => encode_pair(self.values.arm_memory, &mut encoded)?,
                GET_VC_MEMORY => encode_pair(self.values.vc_memory, &mut encoded)?,
                _ => return Err(MailboxError::Unsupported),
            };
            if response.len() > value_capacity {
                return Err(MailboxError::BufferTooSmall);
            }
            buffer[value_start..value_start + value_capacity].fill(0);
            buffer[value_start..value_start + response.len()].copy_from_slice(response);
            write_u32(buffer, cursor + 8, TAG_RESPONSE_BIT | response.len() as u32)?;
            cursor = next;
        }

        write_u32(buffer, 4, RESPONSE_SUCCESS)?;
        self.transactions = self
            .transactions
            .checked_add(1)
            .ok_or(MailboxError::Malformed)?;
        Ok(())
    }
}

#[cfg(any(test, feature = "hosted-dev"))]
fn encode_u32(value: Option<u32>, output: &mut [u8; 8]) -> Result<&[u8], MailboxError> {
    let value = value.ok_or(MailboxError::Unsupported)?;
    output[..4].copy_from_slice(&value.to_le_bytes());
    Ok(&output[..4])
}

#[cfg(any(test, feature = "hosted-dev"))]
fn encode_u64(value: Option<u64>, output: &mut [u8; 8]) -> Result<&[u8], MailboxError> {
    let value = value.ok_or(MailboxError::Unsupported)?;
    output.copy_from_slice(&value.to_le_bytes());
    Ok(output)
}

#[cfg(any(test, feature = "hosted-dev"))]
fn encode_pair(value: Option<(u32, u32)>, output: &mut [u8; 8]) -> Result<&[u8], MailboxError> {
    let (base, size) = value.ok_or(MailboxError::Unsupported)?;
    output[..4].copy_from_slice(&base.to_le_bytes());
    output[4..].copy_from_slice(&size.to_le_bytes());
    Ok(output)
}

#[cfg(any(test, feature = "hosted-dev"))]
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MailboxError> {
    let value = bytes
        .get(offset..offset.checked_add(4).ok_or(MailboxError::Malformed)?)
        .ok_or(MailboxError::Malformed)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(any(test, feature = "hosted-dev"))]
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), MailboxError> {
    let target = bytes
        .get_mut(offset..offset.checked_add(4).ok_or(MailboxError::Malformed)?)
        .ok_or(MailboxError::Malformed)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Marker proving that a future backend received a platform-validated,
/// capability-granted virtual mapping. It does not discover or grant MMIO.
#[cfg(not(feature = "hosted-dev"))]
pub struct GrantedMailboxMapping {
    base: usize,
    length: usize,
}

#[cfg(not(feature = "hosted-dev"))]
impl GrantedMailboxMapping {
    /// # Safety
    /// The caller must prove `base..base + length` is a platform-discovered,
    /// capability-granted mailbox mapping valid for the receiving process.
    pub unsafe fn from_validated_capability(
        base: usize,
        length: usize,
    ) -> Result<Self, MailboxError> {
        if base == 0 || length < 0x24 || base & 3 != 0 {
            return Err(MailboxError::Malformed);
        }
        Ok(Self { base, length })
    }
}

/// Deferred non-hosted transport. It deliberately performs no volatile access
/// until address translation, cache/coherency, and mailbox register policy exist.
#[cfg(not(feature = "hosted-dev"))]
pub struct DeferredMmioTransport {
    mapping: GrantedMailboxMapping,
}

#[cfg(not(feature = "hosted-dev"))]
impl DeferredMmioTransport {
    pub const fn new(mapping: GrantedMailboxMapping) -> Self {
        Self { mapping }
    }

    pub const fn granted_mapping(&self) -> (usize, usize) {
        (self.mapping.base, self.mapping.length)
    }
}

#[cfg(not(feature = "hosted-dev"))]
impl PropertyTransport for DeferredMmioTransport {
    fn transact(&mut self, _buffer: &mut [u8]) -> Result<(), MailboxError> {
        Err(MailboxError::Unsupported)
    }
}
