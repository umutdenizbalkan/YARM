// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Raspberry Pi firmware mailbox property-message wire protocol.
//!
//! This module only packs and validates in-memory property buffers. It does not
//! discover a mailbox, map MMIO, translate addresses, or submit messages to
//! firmware. A firmware-visible property buffer must be 16-byte aligned because
//! the mailbox word combines its upper 28 address bits with a 4-bit channel.

pub const PROPERTY_CHANNEL: u8 = 8;
pub const REQUEST_CODE: u32 = 0x0000_0000;
pub const RESPONSE_SUCCESS: u32 = 0x8000_0000;
pub const RESPONSE_PARSE_ERROR: u32 = 0x8000_0001;
pub const END_TAG: u32 = 0;
pub const TAG_RESPONSE_BIT: u32 = 0x8000_0000;
pub const TAG_LENGTH_MASK: u32 = 0x7fff_ffff;

pub const GET_FIRMWARE_REVISION: u32 = 0x0000_0001;
pub const GET_BOARD_MODEL: u32 = 0x0001_0001;
pub const GET_BOARD_REVISION: u32 = 0x0001_0002;
pub const GET_BOARD_SERIAL: u32 = 0x0001_0004;
pub const GET_ARM_MEMORY: u32 = 0x0001_0005;
pub const GET_VC_MEMORY: u32 = 0x0001_0006;
pub const GET_CLOCK_RATE: u32 = 0x0003_0002;

const HEADER_LEN: usize = 8;
const TAG_HEADER_LEN: usize = 12;
const END_TAG_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxError {
    BufferTooSmall,
    Misaligned,
    Malformed,
    FirmwareError,
    Unsupported,
}

/// Fixed-capacity storage whose first byte is guaranteed to be 16-byte aligned.
#[repr(C, align(16))]
pub struct AlignedPropertyBuffer<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> AlignedPropertyBuffer<N> {
    pub const fn zeroed() -> Self {
        Self { bytes: [0; N] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

pub fn is_16_byte_aligned(bytes: &[u8]) -> bool {
    (bytes.as_ptr() as usize) & 0x0f == 0
}

/// Combines an already translated 32-bit firmware-visible buffer address with
/// a mailbox channel. This does not perform CPU-to-bus address translation.
pub fn encode_mailbox_word(firmware_buffer_address: u32, channel: u8) -> Result<u32, MailboxError> {
    if firmware_buffer_address & 0x0f != 0 {
        return Err(MailboxError::Misaligned);
    }
    if channel > 0x0f {
        return Err(MailboxError::Unsupported);
    }
    Ok(firmware_buffer_address | u32::from(channel))
}

fn padded_len(len: usize) -> Result<usize, MailboxError> {
    len.checked_add(3)
        .map(|value| value & !3)
        .ok_or(MailboxError::BufferTooSmall)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MailboxError> {
    let value = bytes
        .get(offset..offset.checked_add(4).ok_or(MailboxError::Malformed)?)
        .ok_or(MailboxError::Malformed)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), MailboxError> {
    let target = bytes
        .get_mut(offset..offset.checked_add(4).ok_or(MailboxError::BufferTooSmall)?)
        .ok_or(MailboxError::BufferTooSmall)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn supported_tag(tag: u32) -> bool {
    matches!(
        tag,
        GET_FIRMWARE_REVISION
            | GET_BOARD_MODEL
            | GET_BOARD_REVISION
            | GET_BOARD_SERIAL
            | GET_ARM_MEMORY
            | GET_VC_MEMORY
            | GET_CLOCK_RATE
    )
}

/// Bounds-checked request encoder. `finish` leaves a zero end tag in place and
/// writes the exact message size into the header.
pub struct PropertyRequestEncoder<'a> {
    bytes: &'a mut [u8],
    cursor: usize,
}

impl<'a> PropertyRequestEncoder<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Result<Self, MailboxError> {
        if !is_16_byte_aligned(bytes) {
            return Err(MailboxError::Misaligned);
        }
        if bytes.len() < HEADER_LEN + END_TAG_LEN {
            return Err(MailboxError::BufferTooSmall);
        }
        bytes.fill(0);
        write_u32(bytes, 4, REQUEST_CODE)?;
        Ok(Self {
            bytes,
            cursor: HEADER_LEN,
        })
    }

    pub fn push_tag(
        &mut self,
        tag: u32,
        value: &[u8],
        value_buffer_size: usize,
    ) -> Result<(), MailboxError> {
        if tag == END_TAG || !supported_tag(tag) {
            return Err(MailboxError::Unsupported);
        }
        if value.len() > value_buffer_size || value_buffer_size > u32::MAX as usize {
            return Err(MailboxError::Malformed);
        }
        let padded = padded_len(value_buffer_size)?;
        let next = self
            .cursor
            .checked_add(TAG_HEADER_LEN)
            .and_then(|offset| offset.checked_add(padded))
            .ok_or(MailboxError::BufferTooSmall)?;
        let total = next
            .checked_add(END_TAG_LEN)
            .ok_or(MailboxError::BufferTooSmall)?;
        if total > self.bytes.len() {
            return Err(MailboxError::BufferTooSmall);
        }

        write_u32(self.bytes, self.cursor, tag)?;
        write_u32(self.bytes, self.cursor + 4, value_buffer_size as u32)?;
        write_u32(self.bytes, self.cursor + 8, value.len() as u32)?;
        let value_start = self.cursor + TAG_HEADER_LEN;
        self.bytes[value_start..value_start + padded].fill(0);
        self.bytes[value_start..value_start + value.len()].copy_from_slice(value);
        self.cursor = next;
        write_u32(self.bytes, self.cursor, END_TAG)?;
        Ok(())
    }

    pub fn finish(self) -> Result<usize, MailboxError> {
        let total = self
            .cursor
            .checked_add(END_TAG_LEN)
            .ok_or(MailboxError::BufferTooSmall)?;
        if total > u32::MAX as usize {
            return Err(MailboxError::BufferTooSmall);
        }
        write_u32(self.bytes, 0, total as u32)?;
        Ok(total)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyTag<'a> {
    pub id: u32,
    pub value_buffer_size: usize,
    pub value: &'a [u8],
}

/// Validated request view used by mock transports. It never submits MMIO.
pub struct PropertyRequest<'a> {
    bytes: &'a [u8],
    size: usize,
}

impl<'a> PropertyRequest<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MailboxError> {
        let size = validate_message(bytes, REQUEST_CODE)?;
        Ok(Self { bytes, size })
    }

    pub fn tag(&self, wanted: u32) -> Result<PropertyTag<'a>, MailboxError> {
        find_tag(self.bytes, self.size, wanted, false)
    }
}

/// Validated response view. Firmware parse errors and malformed tag response
/// lengths become deterministic errors rather than indexing panics.
pub struct PropertyResponse<'a> {
    bytes: &'a [u8],
    size: usize,
}

impl<'a> PropertyResponse<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MailboxError> {
        if !is_16_byte_aligned(bytes) {
            return Err(MailboxError::Misaligned);
        }
        if bytes.len() < HEADER_LEN + END_TAG_LEN {
            return Err(MailboxError::BufferTooSmall);
        }
        match read_u32(bytes, 4)? {
            RESPONSE_SUCCESS => {}
            RESPONSE_PARSE_ERROR => return Err(MailboxError::FirmwareError),
            REQUEST_CODE => return Err(MailboxError::Malformed),
            _ => return Err(MailboxError::FirmwareError),
        }
        let size = validate_size(bytes)?;
        Ok(Self { bytes, size })
    }

    pub fn tag(&self, wanted: u32) -> Result<PropertyTag<'a>, MailboxError> {
        find_tag(self.bytes, self.size, wanted, true)
    }
}

fn validate_message(bytes: &[u8], expected_code: u32) -> Result<usize, MailboxError> {
    if !is_16_byte_aligned(bytes) {
        return Err(MailboxError::Misaligned);
    }
    if bytes.len() < HEADER_LEN + END_TAG_LEN {
        return Err(MailboxError::BufferTooSmall);
    }
    if read_u32(bytes, 4)? != expected_code {
        return Err(MailboxError::Malformed);
    }
    validate_size(bytes)
}

fn validate_size(bytes: &[u8]) -> Result<usize, MailboxError> {
    let size = read_u32(bytes, 0)? as usize;
    if size < HEADER_LEN + END_TAG_LEN || size > bytes.len() || size & 3 != 0 {
        return Err(MailboxError::Malformed);
    }
    Ok(size)
}

fn find_tag<'a>(
    bytes: &'a [u8],
    size: usize,
    wanted: u32,
    response: bool,
) -> Result<PropertyTag<'a>, MailboxError> {
    if !supported_tag(wanted) {
        return Err(MailboxError::Unsupported);
    }
    let mut cursor = HEADER_LEN;
    loop {
        let id = read_u32(bytes.get(..size).ok_or(MailboxError::Malformed)?, cursor)?;
        if id == END_TAG {
            return Err(MailboxError::Unsupported);
        }
        if !supported_tag(id) {
            return Err(MailboxError::Unsupported);
        }
        let value_buffer_size = read_u32(bytes, cursor + 4)? as usize;
        let length_word = read_u32(bytes, cursor + 8)?;
        let value_len = (length_word & TAG_LENGTH_MASK) as usize;
        if response && length_word & TAG_RESPONSE_BIT == 0 {
            return Err(MailboxError::Malformed);
        }
        if !response && length_word & TAG_RESPONSE_BIT != 0 {
            return Err(MailboxError::Malformed);
        }
        if value_len > value_buffer_size {
            return Err(MailboxError::Malformed);
        }
        let padded = padded_len(value_buffer_size).map_err(|_| MailboxError::Malformed)?;
        let value_start = cursor
            .checked_add(TAG_HEADER_LEN)
            .ok_or(MailboxError::Malformed)?;
        let next = value_start
            .checked_add(padded)
            .ok_or(MailboxError::Malformed)?;
        if next
            .checked_add(END_TAG_LEN)
            .ok_or(MailboxError::Malformed)?
            > size
        {
            return Err(MailboxError::Malformed);
        }
        if id == wanted {
            let value = bytes
                .get(value_start..value_start + value_len)
                .ok_or(MailboxError::Malformed)?;
            return Ok(PropertyTag {
                id,
                value_buffer_size,
                value,
            });
        }
        cursor = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_word(length: u32) -> u32 {
        TAG_RESPONSE_BIT | length
    }

    #[test]
    fn encodes_single_tag_with_header_padding_and_end_tag() {
        let mut storage = AlignedPropertyBuffer::<64>::zeroed();
        let size = {
            let bytes = storage.as_mut_slice();
            let mut encoder = PropertyRequestEncoder::new(bytes).unwrap();
            encoder.push_tag(GET_BOARD_MODEL, &[], 4).unwrap();
            encoder.finish().unwrap()
        };
        assert_eq!(size, 28);
        assert_eq!(read_u32(storage.as_slice(), 0), Ok(28));
        assert_eq!(read_u32(storage.as_slice(), 4), Ok(REQUEST_CODE));
        assert_eq!(read_u32(storage.as_slice(), 8), Ok(GET_BOARD_MODEL));
        assert_eq!(read_u32(storage.as_slice(), 12), Ok(4));
        assert_eq!(read_u32(storage.as_slice(), 24), Ok(END_TAG));
    }

    #[test]
    fn encodes_multiple_tags_and_32_bit_padding() {
        let mut storage = AlignedPropertyBuffer::<96>::zeroed();
        let size = {
            let mut encoder = PropertyRequestEncoder::new(storage.as_mut_slice()).unwrap();
            encoder.push_tag(GET_BOARD_MODEL, &[1, 2, 3], 3).unwrap();
            encoder.push_tag(GET_BOARD_SERIAL, &[], 8).unwrap();
            encoder.finish().unwrap()
        };
        assert_eq!(size, 48);
        assert_eq!(&storage.as_slice()[20..24], &[1, 2, 3, 0]);
        assert_eq!(read_u32(storage.as_slice(), 24), Ok(GET_BOARD_SERIAL));
        assert_eq!(read_u32(storage.as_slice(), 44), Ok(END_TAG));
    }

    #[test]
    fn mailbox_word_combines_upper_address_bits_and_property_channel() {
        assert_eq!(
            encode_mailbox_word(0x1234_5670, PROPERTY_CHANNEL),
            Ok(0x1234_5678)
        );
        assert_eq!(
            encode_mailbox_word(0x1234_5671, PROPERTY_CHANNEL),
            Err(MailboxError::Misaligned)
        );
        assert_eq!(
            encode_mailbox_word(0x1234_5670, 16),
            Err(MailboxError::Unsupported)
        );
    }

    #[test]
    fn rejects_misaligned_storage() {
        let mut raw = [0u8; 65];
        let offset = if is_16_byte_aligned(&raw) { 1 } else { 0 };
        assert_eq!(
            PropertyRequestEncoder::new(&mut raw[offset..offset + 64]).err(),
            Some(MailboxError::Misaligned)
        );
    }

    #[test]
    fn decodes_success_response() {
        let mut storage = AlignedPropertyBuffer::<64>::zeroed();
        let size = {
            let mut encoder = PropertyRequestEncoder::new(storage.as_mut_slice()).unwrap();
            encoder.push_tag(GET_BOARD_REVISION, &[], 4).unwrap();
            encoder.finish().unwrap()
        };
        write_u32(storage.as_mut_slice(), 4, RESPONSE_SUCCESS).unwrap();
        write_u32(storage.as_mut_slice(), 16, response_word(4)).unwrap();
        write_u32(storage.as_mut_slice(), 20, 0x00a0_2082).unwrap();
        let response = PropertyResponse::parse(&storage.as_slice()[..size]).unwrap();
        assert_eq!(
            response.tag(GET_BOARD_REVISION).unwrap().value,
            &0x00a0_2082u32.to_le_bytes()
        );
    }

    #[test]
    fn parse_error_and_unset_response_bit_are_rejected() {
        let mut storage = AlignedPropertyBuffer::<64>::zeroed();
        let size = {
            let mut encoder = PropertyRequestEncoder::new(storage.as_mut_slice()).unwrap();
            encoder.push_tag(GET_BOARD_MODEL, &[], 4).unwrap();
            encoder.finish().unwrap()
        };
        write_u32(storage.as_mut_slice(), 4, RESPONSE_PARSE_ERROR).unwrap();
        assert!(matches!(
            PropertyResponse::parse(&storage.as_slice()[..size]),
            Err(MailboxError::FirmwareError)
        ));
        write_u32(storage.as_mut_slice(), 4, RESPONSE_SUCCESS).unwrap();
        write_u32(storage.as_mut_slice(), 16, 4).unwrap();
        let response = PropertyResponse::parse(&storage.as_slice()[..size]).unwrap();
        assert_eq!(
            response.tag(GET_BOARD_MODEL).err(),
            Some(MailboxError::Malformed)
        );
    }

    #[test]
    fn unknown_tag_and_truncated_buffer_are_rejected_without_panicking() {
        let mut storage = AlignedPropertyBuffer::<64>::zeroed();
        write_u32(storage.as_mut_slice(), 0, 28).unwrap();
        write_u32(storage.as_mut_slice(), 4, RESPONSE_SUCCESS).unwrap();
        write_u32(storage.as_mut_slice(), 8, 0xdead_beef).unwrap();
        write_u32(storage.as_mut_slice(), 12, 4).unwrap();
        write_u32(storage.as_mut_slice(), 16, response_word(4)).unwrap();
        assert_eq!(
            PropertyResponse::parse(&storage.as_slice()[..8]).err(),
            Some(MailboxError::BufferTooSmall)
        );
        let response = PropertyResponse::parse(&storage.as_slice()[..28]).unwrap();
        assert_eq!(
            response.tag(GET_BOARD_MODEL).err(),
            Some(MailboxError::Unsupported)
        );
        write_u32(storage.as_mut_slice(), 0, 64).unwrap();
        assert!(matches!(
            PropertyResponse::parse(&storage.as_slice()[..28]),
            Err(MailboxError::Malformed)
        ));
    }
}
