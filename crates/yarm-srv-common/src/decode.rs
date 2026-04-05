// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    PayloadTooShort { expected: usize, actual: usize },
    ValueOutOfRange,
}

pub fn decode_u64_le(payload: &[u8]) -> Result<u64, DecodeError> {
    if payload.len() < 8 {
        return Err(DecodeError::PayloadTooShort {
            expected: 8,
            actual: payload.len(),
        });
    }
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&payload[..8]);
    Ok(u64::from_le_bytes(raw))
}

pub fn decode_usize_le(payload: &[u8]) -> Result<usize, DecodeError> {
    let value = decode_u64_le(payload)?;
    usize::try_from(value).map_err(|_| DecodeError::ValueOutOfRange)
}

pub fn decode_i32_fd_le(payload: &[u8]) -> Result<i32, DecodeError> {
    let value = decode_u64_le(payload)?;
    i32::try_from(value).map_err(|_| DecodeError::ValueOutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_u64_le_rejects_short_payloads() {
        assert_eq!(
            decode_u64_le(&[1, 2, 3]),
            Err(DecodeError::PayloadTooShort {
                expected: 8,
                actual: 3
            })
        );
    }

    #[test]
    fn decode_usize_le_reports_range_overflow() {
        if usize::BITS == 32 {
            let payload = u64::MAX.to_le_bytes();
            assert_eq!(decode_usize_le(&payload), Err(DecodeError::ValueOutOfRange));
        } else {
            let payload = 42u64.to_le_bytes();
            assert_eq!(decode_usize_le(&payload), Ok(42usize));
        }
    }

    #[test]
    fn decode_i32_fd_le_reports_range_overflow() {
        let payload = (i32::MAX as u64 + 1).to_le_bytes();
        assert_eq!(
            decode_i32_fd_le(&payload),
            Err(DecodeError::ValueOutOfRange)
        );
    }
}
