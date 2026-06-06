// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace-only TCP/IP planning service protocol.
//!
//! This ABI describes deterministic IPv4 route and transmit planning. It does
//! not describe packets, packet queues, checksums, or kernel syscalls.

pub const TCPIP_ABI_VERSION: u16 = 1;
pub const TCPIP_WIRE_LEN: usize = 128;
pub const TCPIP_IPV4_HEADER_ALLOWANCE: u32 = 20;
pub const TCPIP_DEFAULT_TTL: u8 = 64;

pub const TCPIP_OP_ROUTE_IPV4: u16 = 1;
pub const TCPIP_OP_PLAN_SEND_IPV4: u16 = 2;
pub const TCPIP_OP_GET_LOCAL_IPV4: u16 = 3;
pub const TCPIP_OP_SET_DEFAULT_TTL: u16 = 4;
pub const TCPIP_OP_GET_STATUS: u16 = 5;

pub const TCPIP_PROTOCOL_ICMP: u8 = 1;
pub const TCPIP_PROTOCOL_TCP: u8 = 6;
pub const TCPIP_PROTOCOL_UDP: u8 = 17;
pub const TCPIP_PROTOCOL_RAW: u8 = 255;

pub const TCPIP_PLAN_F_SOURCE_EXPLICIT: u16 = 1 << 0;
pub const TCPIP_PLAN_F_ALL: u16 = TCPIP_PLAN_F_SOURCE_EXPLICIT;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpipStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    NoRoute = 3,
    LinkDown = 4,
    NoSourceAddr = 5,
    MtuExceeded = 6,
    InvalidTtl = 7,
    InvalidAddress = 8,
}

impl TcpipStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::NoRoute),
            4 => Some(Self::LinkDown),
            5 => Some(Self::NoSourceAddr),
            6 => Some(Self::MtuExceeded),
            7 => Some(Self::InvalidTtl),
            8 => Some(Self::InvalidAddress),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpipCodecError {
    Malformed,
    UnsupportedOpcode,
    UnsupportedProtocol,
    InvalidTtl,
    InvalidAddress,
    UnsupportedFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4SendSpec {
    pub request_id: u64,
    pub source: u32,
    pub destination: u32,
    pub payload_len: u32,
    pub protocol: u8,
    pub ttl: u8,
    pub flags: u16,
}

impl Ipv4SendSpec {
    pub const fn is_valid(self) -> bool {
        self.request_id != 0
            && valid_optional_source(self.source)
            && valid_destination(self.destination)
            && valid_protocol(self.protocol)
            && self.flags & !TCPIP_PLAN_F_ALL == 0
            && (self.source != 0) == (self.flags & TCPIP_PLAN_F_SOURCE_EXPLICIT != 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpipRequest {
    RouteIpv4 { request_id: u64, destination: u32 },
    PlanSendIpv4 { spec: Ipv4SendSpec },
    GetLocalIpv4 { request_id: u64, device_id: u32 },
    SetDefaultTtl { request_id: u64, ttl: u8 },
    GetStatus { request_id: u64 },
}

impl TcpipRequest {
    pub const ENCODED_LEN: usize = TCPIP_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; Self::ENCODED_LEN]), TcpipCodecError> {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let opcode = match self {
            Self::RouteIpv4 {
                request_id,
                destination,
            } => {
                write_request_id(&mut payload, request_id)?;
                validate_destination(destination)?;
                write_u32(&mut payload, 8, destination);
                TCPIP_OP_ROUTE_IPV4
            }
            Self::PlanSendIpv4 { spec } => {
                validate_send_spec(spec)?;
                write_u64(&mut payload, 0, spec.request_id);
                write_u32(&mut payload, 8, spec.source);
                write_u32(&mut payload, 12, spec.destination);
                write_u32(&mut payload, 16, spec.payload_len);
                payload[20] = spec.protocol;
                payload[21] = spec.ttl;
                write_u16(&mut payload, 22, spec.flags);
                TCPIP_OP_PLAN_SEND_IPV4
            }
            Self::GetLocalIpv4 {
                request_id,
                device_id,
            } => {
                write_request_id(&mut payload, request_id)?;
                require_nonzero(device_id)?;
                write_u32(&mut payload, 8, device_id);
                TCPIP_OP_GET_LOCAL_IPV4
            }
            Self::SetDefaultTtl { request_id, ttl } => {
                write_request_id(&mut payload, request_id)?;
                validate_ttl(ttl)?;
                payload[20] = ttl;
                TCPIP_OP_SET_DEFAULT_TTL
            }
            Self::GetStatus { request_id } => {
                write_request_id(&mut payload, request_id)?;
                TCPIP_OP_GET_STATUS
            }
        };
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, TcpipCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(TcpipCodecError::Malformed);
        }
        match opcode {
            TCPIP_OP_ROUTE_IPV4 => {
                require_zero(&payload[12..])?;
                let destination = read_u32(payload, 8)?;
                validate_destination(destination)?;
                Ok(Self::RouteIpv4 {
                    request_id: read_request_id(payload)?,
                    destination,
                })
            }
            TCPIP_OP_PLAN_SEND_IPV4 => {
                require_zero(&payload[24..])?;
                let spec = Ipv4SendSpec {
                    request_id: read_request_id(payload)?,
                    source: read_u32(payload, 8)?,
                    destination: read_u32(payload, 12)?,
                    payload_len: read_u32(payload, 16)?,
                    protocol: payload[20],
                    ttl: payload[21],
                    flags: read_u16(payload, 22)?,
                };
                validate_send_spec(spec)?;
                Ok(Self::PlanSendIpv4 { spec })
            }
            TCPIP_OP_GET_LOCAL_IPV4 => {
                require_zero(&payload[12..])?;
                Ok(Self::GetLocalIpv4 {
                    request_id: read_request_id(payload)?,
                    device_id: read_nonzero_u32(payload, 8)?,
                })
            }
            TCPIP_OP_SET_DEFAULT_TTL => {
                require_zero(&payload[8..20])?;
                require_zero(&payload[21..])?;
                let ttl = payload[20];
                validate_ttl(ttl)?;
                Ok(Self::SetDefaultTtl {
                    request_id: read_request_id(payload)?,
                    ttl,
                })
            }
            TCPIP_OP_GET_STATUS => {
                require_zero(&payload[8..])?;
                Ok(Self::GetStatus {
                    request_id: read_request_id(payload)?,
                })
            }
            _ => Err(TcpipCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpipResponse {
    pub status: TcpipStatus,
    pub request_id: u64,
    pub route_id: u32,
    pub device_id: u32,
    pub gateway: u32,
    pub source: u32,
    pub destination: u32,
    pub next_hop: u32,
    pub mtu: u32,
    pub payload_len: u32,
    pub effective_ttl: u8,
    pub protocol: u8,
    pub detail: u16,
    pub planned_count: u64,
    pub failed_count: u64,
}

impl TcpipResponse {
    pub const ENCODED_LEN: usize = TCPIP_WIRE_LEN;

    pub const fn status(status: TcpipStatus, request_id: u64) -> Self {
        Self {
            status,
            request_id,
            route_id: 0,
            device_id: 0,
            gateway: 0,
            source: 0,
            destination: 0,
            next_hop: 0,
            mtu: 0,
            payload_len: 0,
            effective_ttl: 0,
            protocol: 0,
            detail: 0,
            planned_count: 0,
            failed_count: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        write_u64(&mut payload, 8, self.request_id);
        write_u32(&mut payload, 16, self.route_id);
        write_u32(&mut payload, 20, self.device_id);
        write_u32(&mut payload, 24, self.gateway);
        write_u32(&mut payload, 28, self.source);
        write_u32(&mut payload, 32, self.destination);
        write_u32(&mut payload, 36, self.next_hop);
        write_u32(&mut payload, 40, self.mtu);
        write_u32(&mut payload, 44, self.payload_len);
        payload[48] = self.effective_ttl;
        payload[49] = self.protocol;
        write_u16(&mut payload, 50, self.detail);
        write_u64(&mut payload, 56, self.planned_count);
        write_u64(&mut payload, 64, self.failed_count);
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, TcpipCodecError> {
        if payload.len() != Self::ENCODED_LEN
            || payload[4..8].iter().any(|byte| *byte != 0)
            || payload[52..56].iter().any(|byte| *byte != 0)
            || payload[72..].iter().any(|byte| *byte != 0)
        {
            return Err(TcpipCodecError::Malformed);
        }
        Ok(Self {
            status: TcpipStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(TcpipCodecError::Malformed)?,
            request_id: read_u64(payload, 8)?,
            route_id: read_u32(payload, 16)?,
            device_id: read_u32(payload, 20)?,
            gateway: read_u32(payload, 24)?,
            source: read_u32(payload, 28)?,
            destination: read_u32(payload, 32)?,
            next_hop: read_u32(payload, 36)?,
            mtu: read_u32(payload, 40)?,
            payload_len: read_u32(payload, 44)?,
            effective_ttl: payload[48],
            protocol: payload[49],
            detail: read_u16(payload, 50)?,
            planned_count: read_u64(payload, 56)?,
            failed_count: read_u64(payload, 64)?,
        })
    }
}

pub const fn valid_destination(address: u32) -> bool {
    address != 0 && address != u32::MAX && address.to_be_bytes()[0] & 0xf0 != 0xe0
}

pub const fn valid_optional_source(address: u32) -> bool {
    address == 0 || valid_destination(address)
}

pub const fn valid_protocol(protocol: u8) -> bool {
    matches!(
        protocol,
        TCPIP_PROTOCOL_ICMP | TCPIP_PROTOCOL_TCP | TCPIP_PROTOCOL_UDP | TCPIP_PROTOCOL_RAW
    )
}

fn validate_send_spec(spec: Ipv4SendSpec) -> Result<(), TcpipCodecError> {
    if !valid_optional_source(spec.source) || !valid_destination(spec.destination) {
        return Err(TcpipCodecError::InvalidAddress);
    }
    if !valid_protocol(spec.protocol) {
        return Err(TcpipCodecError::UnsupportedProtocol);
    }
    validate_ttl(spec.ttl)?;
    if spec.flags & !TCPIP_PLAN_F_ALL != 0
        || (spec.source != 0) != (spec.flags & TCPIP_PLAN_F_SOURCE_EXPLICIT != 0)
        || spec.request_id == 0
    {
        return Err(TcpipCodecError::UnsupportedFlags);
    }
    Ok(())
}

fn validate_destination(destination: u32) -> Result<(), TcpipCodecError> {
    if valid_destination(destination) {
        Ok(())
    } else {
        Err(TcpipCodecError::InvalidAddress)
    }
}

fn validate_ttl(ttl: u8) -> Result<(), TcpipCodecError> {
    if ttl == 0 {
        Err(TcpipCodecError::InvalidTtl)
    } else {
        Ok(())
    }
}

fn write_request_id(payload: &mut [u8], request_id: u64) -> Result<(), TcpipCodecError> {
    if request_id == 0 {
        return Err(TcpipCodecError::Malformed);
    }
    write_u64(payload, 0, request_id);
    Ok(())
}

fn read_request_id(payload: &[u8]) -> Result<u64, TcpipCodecError> {
    let request_id = read_u64(payload, 0)?;
    if request_id == 0 {
        Err(TcpipCodecError::Malformed)
    } else {
        Ok(request_id)
    }
}

fn require_nonzero(value: u32) -> Result<(), TcpipCodecError> {
    if value == 0 {
        Err(TcpipCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn read_nonzero_u32(payload: &[u8], offset: usize) -> Result<u32, TcpipCodecError> {
    let value = read_u32(payload, offset)?;
    require_nonzero(value)?;
    Ok(value)
}

fn require_zero(payload: &[u8]) -> Result<(), TcpipCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(TcpipCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn write_u16(payload: &mut [u8], offset: usize, value: u16) {
    payload[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(payload: &mut [u8], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(payload: &mut [u8], offset: usize, value: u64) {
    payload[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, TcpipCodecError> {
    let bytes = payload
        .get(offset..offset + 2)
        .ok_or(TcpipCodecError::Malformed)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, TcpipCodecError> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or(TcpipCodecError::Malformed)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, TcpipCodecError> {
    let bytes = payload
        .get(offset..offset + 8)
        .ok_or(TcpipCodecError::Malformed)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Ipv4SendSpec {
        Ipv4SendSpec {
            request_id: 7,
            source: u32::from_be_bytes([10, 0, 0, 2]),
            destination: u32::from_be_bytes([10, 0, 0, 9]),
            payload_len: 512,
            protocol: TCPIP_PROTOCOL_UDP,
            ttl: 42,
            flags: TCPIP_PLAN_F_SOURCE_EXPLICIT,
        }
    }

    #[test]
    fn tcpip_request_roundtrips() {
        let requests = [
            TcpipRequest::RouteIpv4 {
                request_id: 1,
                destination: u32::from_be_bytes([10, 0, 0, 9]),
            },
            TcpipRequest::PlanSendIpv4 { spec: spec() },
            TcpipRequest::GetLocalIpv4 {
                request_id: 2,
                device_id: 4,
            },
            TcpipRequest::SetDefaultTtl {
                request_id: 3,
                ttl: 32,
            },
            TcpipRequest::GetStatus { request_id: 4 },
        ];
        for request in requests {
            let (opcode, encoded) = request.encode().expect("encode request");
            assert_eq!(TcpipRequest::decode(opcode, &encoded), Ok(request));
        }
    }

    #[test]
    fn tcpip_response_roundtrips() {
        let response = TcpipResponse {
            status: TcpipStatus::Ok,
            request_id: 7,
            route_id: 3,
            device_id: 4,
            gateway: 0,
            source: u32::from_be_bytes([10, 0, 0, 2]),
            destination: u32::from_be_bytes([10, 0, 0, 9]),
            next_hop: u32::from_be_bytes([10, 0, 0, 9]),
            mtu: 1500,
            payload_len: 512,
            effective_ttl: 42,
            protocol: TCPIP_PROTOCOL_UDP,
            detail: 0,
            planned_count: 1,
            failed_count: 0,
        };
        assert_eq!(TcpipResponse::decode(&response.encode()), Ok(response));
    }

    #[test]
    fn tcpip_rejects_nonzero_reserved_fields() {
        let (opcode, mut encoded) = TcpipRequest::GetStatus { request_id: 1 }
            .encode()
            .expect("encode");
        encoded[127] = 1;
        assert_eq!(
            TcpipRequest::decode(opcode, &encoded),
            Err(TcpipCodecError::Malformed)
        );
    }

    #[test]
    fn tcpip_rejects_unknown_opcode() {
        assert_eq!(
            TcpipRequest::decode(0xffff, &[0; TCPIP_WIRE_LEN]),
            Err(TcpipCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn tcpip_rejects_invalid_ttl_and_addresses() {
        let mut invalid_ttl = spec();
        invalid_ttl.ttl = 0;
        assert_eq!(
            TcpipRequest::PlanSendIpv4 { spec: invalid_ttl }.encode(),
            Err(TcpipCodecError::InvalidTtl)
        );
        let mut invalid_destination = spec();
        invalid_destination.destination = 0;
        assert_eq!(
            TcpipRequest::PlanSendIpv4 {
                spec: invalid_destination
            }
            .encode(),
            Err(TcpipCodecError::InvalidAddress)
        );
        let mut multicast_source = spec();
        multicast_source.source = u32::from_be_bytes([224, 0, 0, 1]);
        assert_eq!(
            TcpipRequest::PlanSendIpv4 {
                spec: multicast_source
            }
            .encode(),
            Err(TcpipCodecError::InvalidAddress)
        );
    }

    #[test]
    fn tcpip_constants_and_statuses_are_stable() {
        assert_eq!(TCPIP_ABI_VERSION, 1);
        assert_eq!(TCPIP_WIRE_LEN, 128);
        assert_eq!(TCPIP_IPV4_HEADER_ALLOWANCE, 20);
        assert_eq!(TCPIP_OP_ROUTE_IPV4, 1);
        assert_eq!(TCPIP_OP_GET_STATUS, 5);
        assert_eq!(TcpipStatus::Ok as u32, 0);
        assert_eq!(TcpipStatus::NoSourceAddr as u32, 5);
        assert_eq!(TcpipStatus::InvalidAddress as u32, 8);
    }
}
