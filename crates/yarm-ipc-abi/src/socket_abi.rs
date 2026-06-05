// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Fixed-size userspace socket service protocol.
//!
//! This ABI does not define kernel socket syscalls and does not imply a real
//! TCP/IP implementation. Version 1 carries a bounded inline payload for the
//! in-memory loopback profile implemented by `socket_srv`.

pub const SOCKET_SERVER_ABI_VERSION: u16 = 1;
pub const SOCKET_CODEC_V1_VERSION: u16 = 1;
pub const SOCKET_WIRE_LEN: usize = 128;
pub const SOCKET_MAX_INLINE_DATA: usize = 64;

pub const SOCKET_OP_CREATE: u16 = 1;
pub const SOCKET_OP_CONNECT: u16 = 2;
pub const SOCKET_OP_SEND: u16 = 3;
pub const SOCKET_OP_CLOSE: u16 = 4;
pub const SOCKET_OP_BIND: u16 = 5;
pub const SOCKET_OP_LISTEN: u16 = 6;
pub const SOCKET_OP_ACCEPT: u16 = 7;
pub const SOCKET_OP_RECV: u16 = 8;
pub const SOCKET_OP_SHUTDOWN: u16 = 9;
pub const SOCKET_OP_GET_STATUS: u16 = 10;

pub const SOCKET_AF_INET: u16 = 1;
pub const SOCKET_AF_LOCAL: u16 = 2;

pub const SOCKET_TYPE_STREAM: u16 = 1;
pub const SOCKET_TYPE_DGRAM: u16 = 2;

pub const SOCKET_PROTOCOL_DEFAULT: u16 = 0;
pub const SOCKET_PROTOCOL_TCP: u16 = 1;
pub const SOCKET_PROTOCOL_UDP: u16 = 2;

pub const SOCKET_LOOPBACK_IPV4: u32 = u32::from_be_bytes([127, 0, 0, 1]);

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    NotFound = 3,
    AlreadyBound = 4,
    NotBound = 5,
    NotConnected = 6,
    WouldBlock = 7,
    Closed = 8,
    TableFull = 9,
    InvalidState = 10,
    MessageTooLarge = 11,
}

impl SocketStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::NotFound),
            4 => Some(Self::AlreadyBound),
            5 => Some(Self::NotBound),
            6 => Some(Self::NotConnected),
            7 => Some(Self::WouldBlock),
            8 => Some(Self::Closed),
            9 => Some(Self::TableFull),
            10 => Some(Self::InvalidState),
            11 => Some(Self::MessageTooLarge),
            _ => None,
        }
    }
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Empty = 0,
    Created = 1,
    Bound = 2,
    Listening = 3,
    Connected = 4,
    Closed = 5,
}

impl SocketState {
    const fn from_wire(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Empty),
            1 => Some(Self::Created),
            2 => Some(Self::Bound),
            3 => Some(Self::Listening),
            4 => Some(Self::Connected),
            5 => Some(Self::Closed),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketShutdown {
    Read = 1,
    Write = 2,
    Both = 3,
}

impl SocketShutdown {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Read),
            2 => Some(Self::Write),
            3 => Some(Self::Both),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketEndpoint {
    pub address: u32,
    pub port: u16,
}

impl SocketEndpoint {
    pub const fn loopback(port: u16) -> Self {
        Self {
            address: SOCKET_LOOPBACK_IPV4,
            port,
        }
    }

    pub const fn is_valid_loopback(self) -> bool {
        self.address == SOCKET_LOOPBACK_IPV4 && self.port != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketCodecError {
    Malformed,
    UnsupportedOpcode,
    InvalidDomain,
    InvalidType,
    InvalidProtocol,
    MessageTooLarge,
}

// Compatibility names retained for existing userspace POSIX adapters. They map
// to the v1 service operations and do not define kernel syscalls.
pub const SOCKET_OP_SOCKET: u16 = SOCKET_OP_CREATE;
pub const SOCKET_OP_SENDTO: u16 = SOCKET_OP_SEND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketArgs {
    pub domain: u64,
    pub sock_type: u64,
    pub protocol: u64,
    pub reserved: u64,
}

impl SocketArgs {
    pub const VERSION: u16 = SOCKET_CODEC_V1_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(domain: u64, sock_type: u64, protocol: u64) -> Self {
        Self {
            domain,
            sock_type,
            protocol,
            reserved: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        encode_u64_values([self.domain, self.sock_type, self.protocol, self.reserved])
    }

    pub fn decode(payload: &[u8]) -> Result<Self, SocketCodecError> {
        let values = decode_u64_values::<4>(payload)?;
        if values[3] != 0 {
            return Err(SocketCodecError::Malformed);
        }
        Ok(Self {
            domain: values[0],
            sock_type: values[1],
            protocol: values[2],
            reserved: values[3],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectArgs {
    pub fd: u64,
    pub addr_ptr: u64,
    pub addr_len: u64,
    pub reserved: u64,
}

impl ConnectArgs {
    pub const VERSION: u16 = SOCKET_CODEC_V1_VERSION;
    pub const ENCODED_LEN: usize = 32;

    pub const fn new(fd: u64, addr_ptr: u64, addr_len: u64) -> Self {
        Self {
            fd,
            addr_ptr,
            addr_len,
            reserved: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        encode_u64_values([self.fd, self.addr_ptr, self.addr_len, self.reserved])
    }

    pub fn decode(payload: &[u8]) -> Result<Self, SocketCodecError> {
        let values = decode_u64_values::<4>(payload)?;
        if values[3] != 0 {
            return Err(SocketCodecError::Malformed);
        }
        Ok(Self {
            fd: values[0],
            addr_ptr: values[1],
            addr_len: values[2],
            reserved: values[3],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendToArgs {
    pub fd: u64,
    pub buf_ptr: u64,
    pub len: u64,
    pub flags: u64,
    pub dest_addr_ptr: u64,
    pub addrlen: u64,
}

impl SendToArgs {
    pub const VERSION: u16 = SOCKET_CODEC_V1_VERSION;
    pub const ENCODED_LEN: usize = 48;

    pub const fn new(
        fd: u64,
        buf_ptr: u64,
        len: u64,
        flags: u64,
        dest_addr_ptr: u64,
        addrlen: u64,
    ) -> Self {
        Self {
            fd,
            buf_ptr,
            len,
            flags,
            dest_addr_ptr,
            addrlen,
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        encode_u64_values([
            self.fd,
            self.buf_ptr,
            self.len,
            self.flags,
            self.dest_addr_ptr,
            self.addrlen,
        ])
    }

    pub fn decode(payload: &[u8]) -> Result<Self, SocketCodecError> {
        let values = decode_u64_values::<6>(payload)?;
        Ok(Self {
            fd: values[0],
            buf_ptr: values[1],
            len: values[2],
            flags: values[3],
            dest_addr_ptr: values[4],
            addrlen: values[5],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketRequest {
    Create {
        domain: u16,
        socket_type: u16,
        protocol: u16,
    },
    Close {
        handle: u32,
    },
    Bind {
        handle: u32,
        endpoint: SocketEndpoint,
    },
    Listen {
        handle: u32,
        backlog: u16,
    },
    Accept {
        handle: u32,
    },
    Connect {
        handle: u32,
        endpoint: SocketEndpoint,
    },
    Send {
        handle: u32,
        len: u16,
        data: [u8; SOCKET_MAX_INLINE_DATA],
    },
    Recv {
        handle: u32,
        max_len: u16,
    },
    Shutdown {
        handle: u32,
        how: SocketShutdown,
    },
    GetStatus {
        handle: u32,
    },
}

impl SocketRequest {
    pub const ENCODED_LEN: usize = SOCKET_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; Self::ENCODED_LEN]), SocketCodecError> {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let opcode = match self {
            Self::Create {
                domain,
                socket_type,
                protocol,
            } => {
                validate_create(domain, socket_type, protocol)?;
                write_u16(&mut payload, 4, domain);
                write_u16(&mut payload, 6, socket_type);
                write_u16(&mut payload, 8, protocol);
                SOCKET_OP_CREATE
            }
            Self::Close { handle } => {
                write_handle(&mut payload, handle)?;
                SOCKET_OP_CLOSE
            }
            Self::Bind { handle, endpoint } => {
                write_handle(&mut payload, handle)?;
                write_endpoint(&mut payload, endpoint)?;
                SOCKET_OP_BIND
            }
            Self::Listen { handle, backlog } => {
                write_handle(&mut payload, handle)?;
                if backlog == 0 {
                    return Err(SocketCodecError::Malformed);
                }
                write_u16(&mut payload, 18, backlog);
                SOCKET_OP_LISTEN
            }
            Self::Accept { handle } => {
                write_handle(&mut payload, handle)?;
                SOCKET_OP_ACCEPT
            }
            Self::Connect { handle, endpoint } => {
                write_handle(&mut payload, handle)?;
                write_endpoint(&mut payload, endpoint)?;
                SOCKET_OP_CONNECT
            }
            Self::Send { handle, len, data } => {
                write_handle(&mut payload, handle)?;
                if usize::from(len) > SOCKET_MAX_INLINE_DATA {
                    return Err(SocketCodecError::MessageTooLarge);
                }
                write_u16(&mut payload, 20, len);
                payload[24..24 + usize::from(len)].copy_from_slice(&data[..usize::from(len)]);
                SOCKET_OP_SEND
            }
            Self::Recv { handle, max_len } => {
                write_handle(&mut payload, handle)?;
                if max_len == 0 || usize::from(max_len) > SOCKET_MAX_INLINE_DATA {
                    return Err(SocketCodecError::MessageTooLarge);
                }
                write_u16(&mut payload, 20, max_len);
                SOCKET_OP_RECV
            }
            Self::Shutdown { handle, how } => {
                write_handle(&mut payload, handle)?;
                payload[10] = how as u8;
                SOCKET_OP_SHUTDOWN
            }
            Self::GetStatus { handle } => {
                write_handle(&mut payload, handle)?;
                SOCKET_OP_GET_STATUS
            }
        };
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, SocketCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(SocketCodecError::Malformed);
        }
        match opcode {
            SOCKET_OP_CREATE => {
                require_zero(&payload[..4])?;
                require_zero(&payload[10..])?;
                let domain = read_u16(payload, 4)?;
                let socket_type = read_u16(payload, 6)?;
                let protocol = read_u16(payload, 8)?;
                validate_create(domain, socket_type, protocol)?;
                Ok(Self::Create {
                    domain,
                    socket_type,
                    protocol,
                })
            }
            SOCKET_OP_CLOSE | SOCKET_OP_ACCEPT | SOCKET_OP_GET_STATUS => {
                require_zero(&payload[4..])?;
                let handle = read_handle(payload)?;
                Ok(match opcode {
                    SOCKET_OP_CLOSE => Self::Close { handle },
                    SOCKET_OP_ACCEPT => Self::Accept { handle },
                    _ => Self::GetStatus { handle },
                })
            }
            SOCKET_OP_BIND | SOCKET_OP_CONNECT => {
                require_zero(&payload[4..12])?;
                require_zero(&payload[18..])?;
                let handle = read_handle(payload)?;
                let endpoint = read_endpoint(payload)?;
                Ok(if opcode == SOCKET_OP_BIND {
                    Self::Bind { handle, endpoint }
                } else {
                    Self::Connect { handle, endpoint }
                })
            }
            SOCKET_OP_LISTEN => {
                require_zero(&payload[4..18])?;
                require_zero(&payload[20..])?;
                let handle = read_handle(payload)?;
                let backlog = read_u16(payload, 18)?;
                if backlog == 0 {
                    return Err(SocketCodecError::Malformed);
                }
                Ok(Self::Listen { handle, backlog })
            }
            SOCKET_OP_SEND => {
                require_zero(&payload[4..20])?;
                require_zero(&payload[22..24])?;
                let handle = read_handle(payload)?;
                let len = read_u16(payload, 20)?;
                if usize::from(len) > SOCKET_MAX_INLINE_DATA {
                    return Err(SocketCodecError::MessageTooLarge);
                }
                require_zero(&payload[24 + usize::from(len)..])?;
                let mut data = [0u8; SOCKET_MAX_INLINE_DATA];
                data[..usize::from(len)].copy_from_slice(&payload[24..24 + usize::from(len)]);
                Ok(Self::Send { handle, len, data })
            }
            SOCKET_OP_RECV => {
                require_zero(&payload[4..20])?;
                require_zero(&payload[22..])?;
                let handle = read_handle(payload)?;
                let max_len = read_u16(payload, 20)?;
                if max_len == 0 || usize::from(max_len) > SOCKET_MAX_INLINE_DATA {
                    return Err(SocketCodecError::MessageTooLarge);
                }
                Ok(Self::Recv { handle, max_len })
            }
            SOCKET_OP_SHUTDOWN => {
                require_zero(&payload[4..10])?;
                require_zero(&payload[11..])?;
                Ok(Self::Shutdown {
                    handle: read_handle(payload)?,
                    how: SocketShutdown::from_wire(payload[10])
                        .ok_or(SocketCodecError::Malformed)?,
                })
            }
            _ => Err(SocketCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketResponse {
    pub status: SocketStatus,
    pub handle: u32,
    pub state: SocketState,
    pub value: u32,
    pub endpoint: Option<SocketEndpoint>,
    pub data_len: u16,
    pub data: [u8; SOCKET_MAX_INLINE_DATA],
}

impl SocketResponse {
    pub const ENCODED_LEN: usize = SOCKET_WIRE_LEN;

    pub const fn status(status: SocketStatus) -> Self {
        Self {
            status,
            handle: 0,
            state: SocketState::Empty,
            value: 0,
            endpoint: None,
            data_len: 0,
            data: [0; SOCKET_MAX_INLINE_DATA],
        }
    }

    pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], SocketCodecError> {
        if usize::from(self.data_len) > SOCKET_MAX_INLINE_DATA {
            return Err(SocketCodecError::MessageTooLarge);
        }
        let mut payload = [0u8; Self::ENCODED_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        write_u32(&mut payload, 4, self.handle);
        write_u16(&mut payload, 8, self.state as u16);
        write_u16(&mut payload, 10, self.data_len);
        write_u32(&mut payload, 12, self.value);
        if let Some(endpoint) = self.endpoint {
            write_u32(&mut payload, 16, endpoint.address);
            write_u16(&mut payload, 20, endpoint.port);
            payload[22] = 1;
        }
        payload[24..24 + usize::from(self.data_len)]
            .copy_from_slice(&self.data[..usize::from(self.data_len)]);
        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, SocketCodecError> {
        if payload.len() != Self::ENCODED_LEN || payload[23] != 0 {
            return Err(SocketCodecError::Malformed);
        }
        let status =
            SocketStatus::from_wire(read_u32(payload, 0)?).ok_or(SocketCodecError::Malformed)?;
        let state =
            SocketState::from_wire(read_u16(payload, 8)?).ok_or(SocketCodecError::Malformed)?;
        let data_len = read_u16(payload, 10)?;
        if usize::from(data_len) > SOCKET_MAX_INLINE_DATA {
            return Err(SocketCodecError::MessageTooLarge);
        }
        let endpoint = match payload[22] {
            0 => {
                if read_u32(payload, 16)? != 0 || read_u16(payload, 20)? != 0 {
                    return Err(SocketCodecError::Malformed);
                }
                None
            }
            1 => {
                let endpoint = SocketEndpoint {
                    address: read_u32(payload, 16)?,
                    port: read_u16(payload, 20)?,
                };
                if !endpoint.is_valid_loopback() {
                    return Err(SocketCodecError::Malformed);
                }
                Some(endpoint)
            }
            _ => return Err(SocketCodecError::Malformed),
        };
        require_zero(&payload[24 + usize::from(data_len)..])?;
        let mut data = [0u8; SOCKET_MAX_INLINE_DATA];
        data[..usize::from(data_len)].copy_from_slice(&payload[24..24 + usize::from(data_len)]);
        Ok(Self {
            status,
            handle: read_u32(payload, 4)?,
            state,
            value: read_u32(payload, 12)?,
            endpoint,
            data_len,
            data,
        })
    }
}

fn validate_create(domain: u16, socket_type: u16, protocol: u16) -> Result<(), SocketCodecError> {
    if !matches!(domain, SOCKET_AF_INET | SOCKET_AF_LOCAL) {
        return Err(SocketCodecError::InvalidDomain);
    }
    if !matches!(socket_type, SOCKET_TYPE_STREAM | SOCKET_TYPE_DGRAM) {
        return Err(SocketCodecError::InvalidType);
    }
    if !matches!(
        protocol,
        SOCKET_PROTOCOL_DEFAULT | SOCKET_PROTOCOL_TCP | SOCKET_PROTOCOL_UDP
    ) {
        return Err(SocketCodecError::InvalidProtocol);
    }
    let compatible = match socket_type {
        SOCKET_TYPE_STREAM => matches!(protocol, SOCKET_PROTOCOL_DEFAULT | SOCKET_PROTOCOL_TCP),
        SOCKET_TYPE_DGRAM => matches!(protocol, SOCKET_PROTOCOL_DEFAULT | SOCKET_PROTOCOL_UDP),
        _ => false,
    };
    if compatible {
        Ok(())
    } else {
        Err(SocketCodecError::InvalidProtocol)
    }
}

fn write_handle(payload: &mut [u8], handle: u32) -> Result<(), SocketCodecError> {
    if handle == 0 {
        return Err(SocketCodecError::Malformed);
    }
    write_u32(payload, 0, handle);
    Ok(())
}

fn read_handle(payload: &[u8]) -> Result<u32, SocketCodecError> {
    let handle = read_u32(payload, 0)?;
    if handle == 0 {
        Err(SocketCodecError::Malformed)
    } else {
        Ok(handle)
    }
}

fn write_endpoint(payload: &mut [u8], endpoint: SocketEndpoint) -> Result<(), SocketCodecError> {
    if !endpoint.is_valid_loopback() {
        return Err(SocketCodecError::Malformed);
    }
    write_u32(payload, 12, endpoint.address);
    write_u16(payload, 16, endpoint.port);
    Ok(())
}

fn read_endpoint(payload: &[u8]) -> Result<SocketEndpoint, SocketCodecError> {
    let endpoint = SocketEndpoint {
        address: read_u32(payload, 12)?,
        port: read_u16(payload, 16)?,
    };
    if endpoint.is_valid_loopback() {
        Ok(endpoint)
    } else {
        Err(SocketCodecError::Malformed)
    }
}

fn require_zero(payload: &[u8]) -> Result<(), SocketCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(SocketCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn encode_u64_values<const N: usize, const BYTES: usize>(values: [u64; N]) -> [u8; BYTES] {
    debug_assert_eq!(BYTES, N * 8);
    let mut payload = [0u8; BYTES];
    for (index, value) in values.into_iter().enumerate() {
        payload[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    payload
}

fn decode_u64_values<const N: usize>(payload: &[u8]) -> Result<[u64; N], SocketCodecError> {
    if payload.len() != N * 8 {
        return Err(SocketCodecError::Malformed);
    }
    let mut values = [0u64; N];
    for (index, value) in values.iter_mut().enumerate() {
        let offset = index * 8;
        let bytes = payload
            .get(offset..offset + 8)
            .ok_or(SocketCodecError::Malformed)?;
        *value = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
    }
    Ok(values)
}

fn write_u16(payload: &mut [u8], offset: usize, value: u16) {
    payload[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(payload: &mut [u8], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, SocketCodecError> {
    let bytes = payload
        .get(offset..offset + 2)
        .ok_or(SocketCodecError::Malformed)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, SocketCodecError> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or(SocketCodecError::Malformed)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(bytes: &[u8]) -> [u8; SOCKET_MAX_INLINE_DATA] {
        let mut data = [0; SOCKET_MAX_INLINE_DATA];
        data[..bytes.len()].copy_from_slice(bytes);
        data
    }

    #[test]
    fn socket_request_roundtrips() {
        let requests = [
            SocketRequest::Create {
                domain: SOCKET_AF_INET,
                socket_type: SOCKET_TYPE_DGRAM,
                protocol: SOCKET_PROTOCOL_UDP,
            },
            SocketRequest::Bind {
                handle: 7,
                endpoint: SocketEndpoint::loopback(8080),
            },
            SocketRequest::Connect {
                handle: 8,
                endpoint: SocketEndpoint::loopback(8080),
            },
            SocketRequest::Send {
                handle: 8,
                len: 4,
                data: data(b"ping"),
            },
            SocketRequest::Recv {
                handle: 7,
                max_len: 64,
            },
            SocketRequest::Shutdown {
                handle: 8,
                how: SocketShutdown::Both,
            },
        ];
        for request in requests {
            let (opcode, encoded) = request.encode().expect("encode request");
            assert_eq!(SocketRequest::decode(opcode, &encoded), Ok(request));
        }
    }

    #[test]
    fn socket_response_roundtrips() {
        let response = SocketResponse {
            status: SocketStatus::Ok,
            handle: 9,
            state: SocketState::Connected,
            value: 4,
            endpoint: Some(SocketEndpoint::loopback(8080)),
            data_len: 4,
            data: data(b"pong"),
        };
        let encoded = response.encode().expect("encode response");
        assert_eq!(SocketResponse::decode(&encoded), Ok(response));
    }

    #[test]
    fn socket_decode_rejects_nonzero_reserved_fields() {
        let (opcode, mut encoded) = SocketRequest::Close { handle: 1 }.encode().expect("encode");
        encoded[127] = 1;
        assert_eq!(
            SocketRequest::decode(opcode, &encoded),
            Err(SocketCodecError::Malformed)
        );

        let mut response = SocketResponse::status(SocketStatus::Ok)
            .encode()
            .expect("encode response");
        response[127] = 1;
        assert_eq!(
            SocketResponse::decode(&response),
            Err(SocketCodecError::Malformed)
        );
    }

    #[test]
    fn socket_decode_rejects_unknown_opcode() {
        assert_eq!(
            SocketRequest::decode(0xffff, &[0; SOCKET_WIRE_LEN]),
            Err(SocketCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn socket_create_rejects_invalid_domain_type_and_protocol() {
        let mut payload = [0; SOCKET_WIRE_LEN];
        write_u16(&mut payload, 4, 99);
        write_u16(&mut payload, 6, SOCKET_TYPE_DGRAM);
        assert_eq!(
            SocketRequest::decode(SOCKET_OP_CREATE, &payload),
            Err(SocketCodecError::InvalidDomain)
        );
        write_u16(&mut payload, 4, SOCKET_AF_INET);
        write_u16(&mut payload, 6, 99);
        assert_eq!(
            SocketRequest::decode(SOCKET_OP_CREATE, &payload),
            Err(SocketCodecError::InvalidType)
        );
        write_u16(&mut payload, 6, SOCKET_TYPE_DGRAM);
        write_u16(&mut payload, 8, SOCKET_PROTOCOL_TCP);
        assert_eq!(
            SocketRequest::decode(SOCKET_OP_CREATE, &payload),
            Err(SocketCodecError::InvalidProtocol)
        );
    }

    #[test]
    fn socket_status_and_operation_constants_are_stable() {
        assert_eq!(SOCKET_SERVER_ABI_VERSION, 1);
        assert_eq!(SOCKET_CODEC_V1_VERSION, 1);
        assert_eq!(SOCKET_WIRE_LEN, 128);
        assert_eq!(SOCKET_MAX_INLINE_DATA, 64);
        assert_eq!(SOCKET_OP_CREATE, 1);
        assert_eq!(SOCKET_OP_GET_STATUS, 10);
        assert_eq!(SocketStatus::Ok as u32, 0);
        assert_eq!(SocketStatus::Unsupported as u32, 2);
        assert_eq!(SocketStatus::MessageTooLarge as u32, 11);
    }
}
