// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace-only DHCP service stub protocol.
//!
//! Version 1 models configuration and lease state only. It does not define
//! DHCP packet construction, UDP transport, or kernel networking.

pub const DHCP_ABI_VERSION: u16 = 1;
pub const DHCP_WIRE_LEN: usize = 128;

pub const DHCP_OP_GET_STATUS: u16 = 1;
pub const DHCP_OP_CONFIGURE_INTERFACE: u16 = 2;
pub const DHCP_OP_START: u16 = 3;
pub const DHCP_OP_STOP: u16 = 4;
pub const DHCP_OP_POLL: u16 = 5;
pub const DHCP_OP_GET_LEASE: u16 = 6;
pub const DHCP_OP_CLEAR_LEASE: u16 = 7;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    NotConfigured = 3,
    AlreadyRunning = 4,
    NotRunning = 5,
    NoLease = 6,
    InvalidDevice = 7,
    InvalidState = 8,
}

impl DhcpStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::NotConfigured),
            4 => Some(Self::AlreadyRunning),
            5 => Some(Self::NotRunning),
            6 => Some(Self::NoLease),
            7 => Some(Self::InvalidDevice),
            8 => Some(Self::InvalidState),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpState {
    Unconfigured = 0,
    Configured = 1,
    Running = 2,
    Stopped = 3,
}

impl DhcpState {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Unconfigured),
            1 => Some(Self::Configured),
            2 => Some(Self::Running),
            3 => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpCodecError {
    Malformed,
    UnsupportedOpcode,
    InvalidDevice,
    InvalidLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpInterfaceConfig {
    pub device_id: u32,
    pub owner_id: u64,
    pub generation: u32,
}

impl DhcpInterfaceConfig {
    pub const fn is_valid(self) -> bool {
        self.device_id != 0 && self.owner_id != 0 && self.generation != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpLease {
    pub device_id: u32,
    pub generation: u32,
    pub assigned_ipv4: u32,
    pub prefix_len: u8,
    pub gateway_ipv4: u32,
    pub dns_server_ipv4: u32,
    pub lease_seconds: u32,
}

impl DhcpLease {
    pub const fn is_valid(self) -> bool {
        self.device_id != 0
            && self.generation != 0
            && self.assigned_ipv4 != 0
            && self.prefix_len <= 32
            && self.lease_seconds != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpRequest {
    GetStatus {
        request_id: u64,
    },
    ConfigureInterface {
        request_id: u64,
        config: DhcpInterfaceConfig,
    },
    Start {
        request_id: u64,
    },
    Stop {
        request_id: u64,
    },
    Poll {
        request_id: u64,
        timeout_hint: u32,
    },
    GetLease {
        request_id: u64,
    },
    ClearLease {
        request_id: u64,
    },
}

impl DhcpRequest {
    pub const ENCODED_LEN: usize = DHCP_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; DHCP_WIRE_LEN]), DhcpCodecError> {
        let mut payload = [0u8; DHCP_WIRE_LEN];
        let (opcode, request_id) = match self {
            Self::GetStatus { request_id } => (DHCP_OP_GET_STATUS, request_id),
            Self::ConfigureInterface { request_id, config } => {
                if !config.is_valid() {
                    return Err(DhcpCodecError::InvalidDevice);
                }
                write_u32(&mut payload, 8, config.device_id);
                write_u64(&mut payload, 12, config.owner_id);
                write_u32(&mut payload, 20, config.generation);
                (DHCP_OP_CONFIGURE_INTERFACE, request_id)
            }
            Self::Start { request_id } => (DHCP_OP_START, request_id),
            Self::Stop { request_id } => (DHCP_OP_STOP, request_id),
            Self::Poll {
                request_id,
                timeout_hint,
            } => {
                write_u32(&mut payload, 8, timeout_hint);
                (DHCP_OP_POLL, request_id)
            }
            Self::GetLease { request_id } => (DHCP_OP_GET_LEASE, request_id),
            Self::ClearLease { request_id } => (DHCP_OP_CLEAR_LEASE, request_id),
        };
        require_request_id(request_id)?;
        write_u64(&mut payload, 0, request_id);
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, DhcpCodecError> {
        if payload.len() != DHCP_WIRE_LEN {
            return Err(DhcpCodecError::Malformed);
        }
        if !matches!(
            opcode,
            DHCP_OP_GET_STATUS
                | DHCP_OP_CONFIGURE_INTERFACE
                | DHCP_OP_START
                | DHCP_OP_STOP
                | DHCP_OP_POLL
                | DHCP_OP_GET_LEASE
                | DHCP_OP_CLEAR_LEASE
        ) {
            return Err(DhcpCodecError::UnsupportedOpcode);
        }
        let request_id = read_request_id(payload)?;
        match opcode {
            DHCP_OP_GET_STATUS | DHCP_OP_START | DHCP_OP_STOP | DHCP_OP_GET_LEASE
            | DHCP_OP_CLEAR_LEASE => {
                require_zero(&payload[8..])?;
                Ok(match opcode {
                    DHCP_OP_GET_STATUS => Self::GetStatus { request_id },
                    DHCP_OP_START => Self::Start { request_id },
                    DHCP_OP_STOP => Self::Stop { request_id },
                    DHCP_OP_GET_LEASE => Self::GetLease { request_id },
                    _ => Self::ClearLease { request_id },
                })
            }
            DHCP_OP_CONFIGURE_INTERFACE => {
                require_zero(&payload[24..])?;
                let config = DhcpInterfaceConfig {
                    device_id: read_u32(payload, 8)?,
                    owner_id: read_u64(payload, 12)?,
                    generation: read_u32(payload, 20)?,
                };
                if !config.is_valid() {
                    return Err(DhcpCodecError::InvalidDevice);
                }
                Ok(Self::ConfigureInterface { request_id, config })
            }
            DHCP_OP_POLL => {
                require_zero(&payload[12..])?;
                Ok(Self::Poll {
                    request_id,
                    timeout_hint: read_u32(payload, 8)?,
                })
            }
            _ => Err(DhcpCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpResponse {
    pub status: DhcpStatus,
    pub state: DhcpState,
    pub request_id: u64,
    pub config: Option<DhcpInterfaceConfig>,
    pub lease: Option<DhcpLease>,
    pub polls: u64,
}

impl DhcpResponse {
    pub const ENCODED_LEN: usize = DHCP_WIRE_LEN;

    pub const fn status(status: DhcpStatus, state: DhcpState, request_id: u64) -> Self {
        Self {
            status,
            state,
            request_id,
            config: None,
            lease: None,
            polls: 0,
        }
    }

    pub fn encode(self) -> Result<[u8; DHCP_WIRE_LEN], DhcpCodecError> {
        let mut payload = [0u8; DHCP_WIRE_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        payload[4] = self.state as u8;
        payload[5] = u8::from(self.config.is_some());
        payload[6] = u8::from(self.lease.is_some());
        write_u64(&mut payload, 8, self.request_id);
        if let Some(config) = self.config {
            if !config.is_valid() {
                return Err(DhcpCodecError::InvalidDevice);
            }
            write_u32(&mut payload, 16, config.device_id);
            write_u64(&mut payload, 20, config.owner_id);
            write_u32(&mut payload, 28, config.generation);
        }
        if let Some(lease) = self.lease {
            if !lease.is_valid() {
                return Err(DhcpCodecError::InvalidLease);
            }
            encode_lease(lease, &mut payload[32..60]);
        }
        write_u64(&mut payload, 64, self.polls);
        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, DhcpCodecError> {
        if payload.len() != DHCP_WIRE_LEN
            || payload[7] != 0
            || payload[60..64].iter().any(|byte| *byte != 0)
            || payload[72..].iter().any(|byte| *byte != 0)
        {
            return Err(DhcpCodecError::Malformed);
        }
        let has_config = decode_bool(payload[5])?;
        let has_lease = decode_bool(payload[6])?;
        let config = if has_config {
            let config = DhcpInterfaceConfig {
                device_id: read_u32(payload, 16)?,
                owner_id: read_u64(payload, 20)?,
                generation: read_u32(payload, 28)?,
            };
            if !config.is_valid() {
                return Err(DhcpCodecError::InvalidDevice);
            }
            Some(config)
        } else {
            require_zero(&payload[16..32])?;
            None
        };
        let lease = if has_lease {
            Some(decode_lease(&payload[32..60])?)
        } else {
            require_zero(&payload[32..60])?;
            None
        };
        Ok(Self {
            status: DhcpStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(DhcpCodecError::Malformed)?,
            state: DhcpState::from_wire(payload[4]).ok_or(DhcpCodecError::Malformed)?,
            request_id: read_u64(payload, 8)?,
            config,
            lease,
            polls: read_u64(payload, 64)?,
        })
    }
}

fn encode_lease(lease: DhcpLease, payload: &mut [u8]) {
    write_u32(payload, 0, lease.device_id);
    write_u32(payload, 4, lease.generation);
    write_u32(payload, 8, lease.assigned_ipv4);
    payload[12] = lease.prefix_len;
    write_u32(payload, 16, lease.gateway_ipv4);
    write_u32(payload, 20, lease.dns_server_ipv4);
    write_u32(payload, 24, lease.lease_seconds);
}

fn decode_lease(payload: &[u8]) -> Result<DhcpLease, DhcpCodecError> {
    if payload.len() != 28 || payload[13..16].iter().any(|byte| *byte != 0) {
        return Err(DhcpCodecError::Malformed);
    }
    let lease = DhcpLease {
        device_id: read_u32(payload, 0)?,
        generation: read_u32(payload, 4)?,
        assigned_ipv4: read_u32(payload, 8)?,
        prefix_len: payload[12],
        gateway_ipv4: read_u32(payload, 16)?,
        dns_server_ipv4: read_u32(payload, 20)?,
        lease_seconds: read_u32(payload, 24)?,
    };
    if lease.is_valid() {
        Ok(lease)
    } else {
        Err(DhcpCodecError::InvalidLease)
    }
}

fn require_request_id(value: u64) -> Result<(), DhcpCodecError> {
    if value == 0 {
        Err(DhcpCodecError::Malformed)
    } else {
        Ok(())
    }
}
fn read_request_id(payload: &[u8]) -> Result<u64, DhcpCodecError> {
    read_request_id_at(payload, 0)
}
fn read_request_id_at(payload: &[u8], offset: usize) -> Result<u64, DhcpCodecError> {
    let value = read_u64(payload, offset)?;
    require_request_id(value)?;
    Ok(value)
}
fn require_zero(payload: &[u8]) -> Result<(), DhcpCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(DhcpCodecError::Malformed)
    } else {
        Ok(())
    }
}
fn decode_bool(value: u8) -> Result<bool, DhcpCodecError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DhcpCodecError::Malformed),
    }
}
fn write_u32(payload: &mut [u8], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn write_u64(payload: &mut [u8], offset: usize, value: u64) {
    payload[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn read_u32(payload: &[u8], offset: usize) -> Result<u32, DhcpCodecError> {
    let b = payload
        .get(offset..offset + 4)
        .ok_or(DhcpCodecError::Malformed)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn read_u64(payload: &[u8], offset: usize) -> Result<u64, DhcpCodecError> {
    let b = payload
        .get(offset..offset + 8)
        .ok_or(DhcpCodecError::Malformed)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: DhcpInterfaceConfig = DhcpInterfaceConfig {
        device_id: 2,
        owner_id: 9,
        generation: 3,
    };
    const LEASE: DhcpLease = DhcpLease {
        device_id: 2,
        generation: 3,
        assigned_ipv4: u32::from_be_bytes([192, 0, 2, 10]),
        prefix_len: 24,
        gateway_ipv4: u32::from_be_bytes([192, 0, 2, 1]),
        dns_server_ipv4: u32::from_be_bytes([192, 0, 2, 53]),
        lease_seconds: 3600,
    };

    #[test]
    fn dhcp_request_roundtrips() {
        let requests = [
            DhcpRequest::GetStatus { request_id: 1 },
            DhcpRequest::ConfigureInterface {
                request_id: 2,
                config: CONFIG,
            },
            DhcpRequest::Start { request_id: 3 },
            DhcpRequest::Stop { request_id: 4 },
            DhcpRequest::Poll {
                request_id: 5,
                timeout_hint: 7,
            },
            DhcpRequest::GetLease { request_id: 6 },
            DhcpRequest::ClearLease { request_id: 7 },
        ];
        for request in requests {
            let (opcode, bytes) = request.encode().expect("encode");
            assert_eq!(DhcpRequest::decode(opcode, &bytes), Ok(request));
        }
    }

    #[test]
    fn dhcp_response_roundtrips() {
        let response = DhcpResponse {
            status: DhcpStatus::Ok,
            state: DhcpState::Running,
            request_id: 8,
            config: Some(CONFIG),
            lease: Some(LEASE),
            polls: 4,
        };
        let bytes = response.encode().expect("encode");
        assert_eq!(DhcpResponse::decode(&bytes), Ok(response));
    }

    #[test]
    fn dhcp_rejects_reserved_and_unknown() {
        let (opcode, mut bytes) = DhcpRequest::Start { request_id: 1 }
            .encode()
            .expect("encode");
        bytes[127] = 1;
        assert_eq!(
            DhcpRequest::decode(opcode, &bytes),
            Err(DhcpCodecError::Malformed)
        );
        assert_eq!(
            DhcpRequest::decode(0xffff, &[0; DHCP_WIRE_LEN]),
            Err(DhcpCodecError::UnsupportedOpcode)
        );
        let mut valid = [0u8; DHCP_WIRE_LEN];
        valid[..8].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            DhcpRequest::decode(0xffff, &valid),
            Err(DhcpCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn dhcp_constants_are_stable() {
        assert_eq!(DHCP_ABI_VERSION, 1);
        assert_eq!(DHCP_WIRE_LEN, 128);
        assert_eq!(DHCP_OP_GET_STATUS, 1);
        assert_eq!(DHCP_OP_CLEAR_LEASE, 7);
        assert_eq!(DhcpStatus::Ok as u32, 0);
        assert_eq!(DhcpStatus::NoLease as u32, 6);
        assert_eq!(DhcpState::Running as u8, 2);
    }
}
