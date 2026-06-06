// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace-only DNS resolver stub protocol.
//!
//! Version 1 models server configuration and bounded query intent only. It
//! does not define DNS wire packets, UDP transport, retries, or cache policy.

pub const DNS_ABI_VERSION: u16 = 1;
pub const DNS_WIRE_LEN: usize = 128;
pub const DNS_MAX_NAME_LEN: usize = 88;

pub const DNS_OP_GET_STATUS: u16 = 1;
pub const DNS_OP_CONFIGURE_SERVER: u16 = 2;
pub const DNS_OP_CLEAR_SERVER: u16 = 3;
pub const DNS_OP_QUERY_A: u16 = 4;
pub const DNS_OP_QUERY_AAAA: u16 = 5;
pub const DNS_OP_QUERY_PTR: u16 = 6;
pub const DNS_OP_CLEAR_CACHE: u16 = 7;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    NotConfigured = 3,
    NoAnswer = 4,
    NameTooLong = 5,
    InvalidName = 6,
    InvalidServer = 7,
    CacheEmpty = 8,
}

impl DnsStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::NotConfigured),
            4 => Some(Self::NoAnswer),
            5 => Some(Self::NameTooLong),
            6 => Some(Self::InvalidName),
            7 => Some(Self::InvalidServer),
            8 => Some(Self::CacheEmpty),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsCodecError {
    Malformed,
    UnsupportedOpcode,
    NameTooLong,
    InvalidName,
    InvalidServer,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsQueryKind {
    A = 1,
    Aaaa = 2,
    Ptr = 3,
}

impl DnsQueryKind {
    const fn opcode(self) -> u16 {
        match self {
            Self::A => DNS_OP_QUERY_A,
            Self::Aaaa => DNS_OP_QUERY_AAAA,
            Self::Ptr => DNS_OP_QUERY_PTR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsName {
    len: u8,
    bytes: [u8; DNS_MAX_NAME_LEN],
}

impl DnsName {
    pub fn new(name: &[u8]) -> Result<Self, DnsCodecError> {
        validate_dns_name(name)?;
        let mut bytes = [0u8; DNS_MAX_NAME_LEN];
        bytes[..name.len()].copy_from_slice(name);
        Ok(Self {
            len: name.len() as u8,
            bytes,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsRequest {
    GetStatus {
        request_id: u64,
    },
    ConfigureServer {
        request_id: u64,
        server_ipv4: u32,
    },
    ClearServer {
        request_id: u64,
    },
    Query {
        request_id: u64,
        kind: DnsQueryKind,
        name: DnsName,
    },
    ClearCache {
        request_id: u64,
    },
}

impl DnsRequest {
    pub const ENCODED_LEN: usize = DNS_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; DNS_WIRE_LEN]), DnsCodecError> {
        let mut payload = [0u8; DNS_WIRE_LEN];
        let (opcode, request_id) = match self {
            Self::GetStatus { request_id } => (DNS_OP_GET_STATUS, request_id),
            Self::ConfigureServer {
                request_id,
                server_ipv4,
            } => {
                if !valid_dns_server(server_ipv4) {
                    return Err(DnsCodecError::InvalidServer);
                }
                write_u32(&mut payload, 8, server_ipv4);
                (DNS_OP_CONFIGURE_SERVER, request_id)
            }
            Self::ClearServer { request_id } => (DNS_OP_CLEAR_SERVER, request_id),
            Self::Query {
                request_id,
                kind,
                name,
            } => {
                payload[16] = name.len;
                payload[20..20 + name.as_bytes().len()].copy_from_slice(name.as_bytes());
                (kind.opcode(), request_id)
            }
            Self::ClearCache { request_id } => (DNS_OP_CLEAR_CACHE, request_id),
        };
        require_request_id(request_id)?;
        write_u64(&mut payload, 0, request_id);
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, DnsCodecError> {
        if payload.len() != DNS_WIRE_LEN {
            return Err(DnsCodecError::Malformed);
        }
        if !matches!(
            opcode,
            DNS_OP_GET_STATUS
                | DNS_OP_CONFIGURE_SERVER
                | DNS_OP_CLEAR_SERVER
                | DNS_OP_QUERY_A
                | DNS_OP_QUERY_AAAA
                | DNS_OP_QUERY_PTR
                | DNS_OP_CLEAR_CACHE
        ) {
            return Err(DnsCodecError::UnsupportedOpcode);
        }
        let request_id = read_request_id(payload)?;
        match opcode {
            DNS_OP_GET_STATUS | DNS_OP_CLEAR_SERVER | DNS_OP_CLEAR_CACHE => {
                require_zero(&payload[8..])?;
                Ok(match opcode {
                    DNS_OP_GET_STATUS => Self::GetStatus { request_id },
                    DNS_OP_CLEAR_SERVER => Self::ClearServer { request_id },
                    _ => Self::ClearCache { request_id },
                })
            }
            DNS_OP_CONFIGURE_SERVER => {
                require_zero(&payload[12..])?;
                let server_ipv4 = read_u32(payload, 8)?;
                if !valid_dns_server(server_ipv4) {
                    return Err(DnsCodecError::InvalidServer);
                }
                Ok(Self::ConfigureServer {
                    request_id,
                    server_ipv4,
                })
            }
            DNS_OP_QUERY_A | DNS_OP_QUERY_AAAA | DNS_OP_QUERY_PTR => {
                require_zero(&payload[8..16])?;
                require_zero(&payload[17..20])?;
                let len = usize::from(payload[16]);
                if len > DNS_MAX_NAME_LEN {
                    return Err(DnsCodecError::NameTooLong);
                }
                require_zero(&payload[20 + len..])?;
                let name = DnsName::new(&payload[20..20 + len])?;
                let kind = match opcode {
                    DNS_OP_QUERY_A => DnsQueryKind::A,
                    DNS_OP_QUERY_AAAA => DnsQueryKind::Aaaa,
                    _ => DnsQueryKind::Ptr,
                };
                Ok(Self::Query {
                    request_id,
                    kind,
                    name,
                })
            }
            _ => Err(DnsCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsResponse {
    pub status: DnsStatus,
    pub request_id: u64,
    pub server_ipv4: u32,
    pub answer_ipv4: u32,
    pub answer_ipv6: [u8; 16],
    pub ttl: u32,
    pub cached: bool,
    pub queries: u64,
}

impl DnsResponse {
    pub const ENCODED_LEN: usize = DNS_WIRE_LEN;

    pub const fn status(status: DnsStatus, request_id: u64) -> Self {
        Self {
            status,
            request_id,
            server_ipv4: 0,
            answer_ipv4: 0,
            answer_ipv6: [0; 16],
            ttl: 0,
            cached: false,
            queries: 0,
        }
    }

    pub fn encode(self) -> Result<[u8; DNS_WIRE_LEN], DnsCodecError> {
        if self.server_ipv4 != 0 && !valid_dns_server(self.server_ipv4) {
            return Err(DnsCodecError::InvalidServer);
        }
        let mut payload = [0u8; DNS_WIRE_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        payload[4] = u8::from(self.cached);
        write_u64(&mut payload, 8, self.request_id);
        write_u32(&mut payload, 16, self.server_ipv4);
        write_u32(&mut payload, 20, self.answer_ipv4);
        payload[24..40].copy_from_slice(&self.answer_ipv6);
        write_u32(&mut payload, 40, self.ttl);
        write_u64(&mut payload, 48, self.queries);
        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, DnsCodecError> {
        if payload.len() != DNS_WIRE_LEN
            || payload[5..8].iter().any(|byte| *byte != 0)
            || payload[44..48].iter().any(|byte| *byte != 0)
            || payload[56..].iter().any(|byte| *byte != 0)
        {
            return Err(DnsCodecError::Malformed);
        }
        let server_ipv4 = read_u32(payload, 16)?;
        if server_ipv4 != 0 && !valid_dns_server(server_ipv4) {
            return Err(DnsCodecError::InvalidServer);
        }
        let mut answer_ipv6 = [0u8; 16];
        answer_ipv6.copy_from_slice(&payload[24..40]);
        Ok(Self {
            status: DnsStatus::from_wire(read_u32(payload, 0)?).ok_or(DnsCodecError::Malformed)?,
            request_id: read_u64(payload, 8)?,
            server_ipv4,
            answer_ipv4: read_u32(payload, 20)?,
            answer_ipv6,
            ttl: read_u32(payload, 40)?,
            cached: decode_bool(payload[4])?,
            queries: read_u64(payload, 48)?,
        })
    }
}

pub fn validate_dns_name(name: &[u8]) -> Result<(), DnsCodecError> {
    if name.is_empty() {
        return Err(DnsCodecError::InvalidName);
    }
    if name.len() > DNS_MAX_NAME_LEN {
        return Err(DnsCodecError::NameTooLong);
    }
    let mut label_len = 0usize;
    let mut label_first = 0u8;
    let mut previous = 0u8;
    for &byte in name {
        if byte == b'.' {
            if label_len == 0 || previous == b'-' {
                return Err(DnsCodecError::InvalidName);
            }
            label_len = 0;
            label_first = 0;
            previous = byte;
            continue;
        }
        if !(byte.is_ascii_alphanumeric() || byte == b'-') {
            return Err(DnsCodecError::InvalidName);
        }
        if label_len == 0 {
            label_first = byte;
            if label_first == b'-' {
                return Err(DnsCodecError::InvalidName);
            }
        }
        label_len += 1;
        if label_len > 63 {
            return Err(DnsCodecError::InvalidName);
        }
        previous = byte;
    }
    if label_len == 0 || label_first == b'-' || previous == b'-' {
        Err(DnsCodecError::InvalidName)
    } else {
        Ok(())
    }
}

pub const fn valid_dns_server(address: u32) -> bool {
    let octets = address.to_be_bytes();
    address != 0 && address != u32::MAX && octets[0] != 0 && octets[0] < 224
}

fn require_request_id(value: u64) -> Result<(), DnsCodecError> {
    if value == 0 {
        Err(DnsCodecError::Malformed)
    } else {
        Ok(())
    }
}
fn read_request_id(payload: &[u8]) -> Result<u64, DnsCodecError> {
    read_request_id_at(payload, 0)
}
fn read_request_id_at(payload: &[u8], offset: usize) -> Result<u64, DnsCodecError> {
    let value = read_u64(payload, offset)?;
    require_request_id(value)?;
    Ok(value)
}
fn require_zero(payload: &[u8]) -> Result<(), DnsCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(DnsCodecError::Malformed)
    } else {
        Ok(())
    }
}
fn decode_bool(value: u8) -> Result<bool, DnsCodecError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DnsCodecError::Malformed),
    }
}
fn write_u32(payload: &mut [u8], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn write_u64(payload: &mut [u8], offset: usize, value: u64) {
    payload[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn read_u32(payload: &[u8], offset: usize) -> Result<u32, DnsCodecError> {
    let b = payload
        .get(offset..offset + 4)
        .ok_or(DnsCodecError::Malformed)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn read_u64(payload: &[u8], offset: usize) -> Result<u64, DnsCodecError> {
    let b = payload
        .get(offset..offset + 8)
        .ok_or(DnsCodecError::Malformed)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> DnsName {
        DnsName::new(b"example.test").expect("name")
    }

    #[test]
    fn dns_request_roundtrips() {
        let requests = [
            DnsRequest::GetStatus { request_id: 1 },
            DnsRequest::ConfigureServer {
                request_id: 2,
                server_ipv4: u32::from_be_bytes([192, 0, 2, 53]),
            },
            DnsRequest::ClearServer { request_id: 3 },
            DnsRequest::Query {
                request_id: 4,
                kind: DnsQueryKind::A,
                name: name(),
            },
            DnsRequest::Query {
                request_id: 5,
                kind: DnsQueryKind::Aaaa,
                name: name(),
            },
            DnsRequest::Query {
                request_id: 6,
                kind: DnsQueryKind::Ptr,
                name: name(),
            },
            DnsRequest::ClearCache { request_id: 7 },
        ];
        for request in requests {
            let (opcode, bytes) = request.encode().expect("encode");
            assert_eq!(DnsRequest::decode(opcode, &bytes), Ok(request));
        }
    }

    #[test]
    fn dns_response_roundtrips() {
        let response = DnsResponse {
            status: DnsStatus::Ok,
            request_id: 8,
            server_ipv4: u32::from_be_bytes([192, 0, 2, 53]),
            answer_ipv4: u32::from_be_bytes([192, 0, 2, 80]),
            answer_ipv6: [1; 16],
            ttl: 60,
            cached: true,
            queries: 2,
        };
        let bytes = response.encode().expect("encode");
        assert_eq!(DnsResponse::decode(&bytes), Ok(response));
    }

    #[test]
    fn dns_rejects_reserved_and_unknown() {
        let (opcode, mut bytes) = DnsRequest::Query {
            request_id: 1,
            kind: DnsQueryKind::A,
            name: name(),
        }
        .encode()
        .expect("encode");
        bytes[19] = 1;
        assert_eq!(
            DnsRequest::decode(opcode, &bytes),
            Err(DnsCodecError::Malformed)
        );
        let mut valid = [0u8; DNS_WIRE_LEN];
        valid[..8].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            DnsRequest::decode(0xffff, &valid),
            Err(DnsCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn dns_name_validation_is_strict() {
        for valid in [b"a".as_slice(), b"example.test", b"a1-b2.example"] {
            assert_eq!(validate_dns_name(valid), Ok(()));
        }
        for invalid in [
            b"".as_slice(),
            b".bad",
            b"bad.",
            b"two..dots",
            b"-bad",
            b"bad-",
            b"bad_name",
        ] {
            assert_eq!(validate_dns_name(invalid), Err(DnsCodecError::InvalidName));
        }
        let long_label = [b'a'; 64];
        assert_eq!(
            validate_dns_name(&long_label),
            Err(DnsCodecError::InvalidName)
        );
        let too_long = [b'a'; DNS_MAX_NAME_LEN + 1];
        assert_eq!(
            validate_dns_name(&too_long),
            Err(DnsCodecError::NameTooLong)
        );
    }

    #[test]
    fn dns_constants_are_stable() {
        assert_eq!(DNS_ABI_VERSION, 1);
        assert_eq!(DNS_WIRE_LEN, 128);
        assert_eq!(DNS_MAX_NAME_LEN, 88);
        assert_eq!(DNS_OP_GET_STATUS, 1);
        assert_eq!(DNS_OP_CLEAR_CACHE, 7);
        assert_eq!(DnsStatus::Ok as u32, 0);
        assert_eq!(DnsStatus::NoAnswer as u32, 4);
    }
}
