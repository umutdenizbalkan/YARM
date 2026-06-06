// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace-only network-manager registry protocol.
//!
//! This ABI describes bounded device, IPv4-address, and route metadata. It
//! does not define a kernel ABI or any packet transport.

pub const NETMGR_ABI_VERSION: u16 = 1;
pub const NETMGR_WIRE_LEN: usize = 128;

pub const NETMGR_OP_REGISTER_DEVICE: u16 = 1;
pub const NETMGR_OP_UNREGISTER_DEVICE: u16 = 2;
pub const NETMGR_OP_GET_DEVICE: u16 = 3;
pub const NETMGR_OP_LIST_DEVICES: u16 = 4;
pub const NETMGR_OP_SET_LINK_STATE: u16 = 5;
pub const NETMGR_OP_ADD_IPV4_ADDR: u16 = 6;
pub const NETMGR_OP_REMOVE_IPV4_ADDR: u16 = 7;
pub const NETMGR_OP_ADD_ROUTE: u16 = 8;
pub const NETMGR_OP_REMOVE_ROUTE: u16 = 9;
pub const NETMGR_OP_LOOKUP_ROUTE: u16 = 10;
pub const NETMGR_OP_GET_STATUS: u16 = 11;
pub const NETMGR_OP_GET_FIRST_IPV4_ADDR_FOR_DEVICE: u16 = 12;
pub const NETMGR_OP_CHECK_IPV4_ADDR_ON_DEVICE: u16 = 13;

pub const NET_DEVICE_FLAG_BROADCAST: u32 = 1 << 0;
pub const NET_DEVICE_FLAG_MULTICAST: u32 = 1 << 1;
pub const NET_DEVICE_FLAG_VIRTUAL: u32 = 1 << 2;
pub const NET_DEVICE_FLAG_LOOPBACK: u32 = 1 << 3;
pub const NET_DEVICE_FLAG_ALL: u32 = NET_DEVICE_FLAG_BROADCAST
    | NET_DEVICE_FLAG_MULTICAST
    | NET_DEVICE_FLAG_VIRTUAL
    | NET_DEVICE_FLAG_LOOPBACK;

pub const NETMGR_DEVICE_ID_LOOPBACK: u32 = 1;
pub const NETMGR_ROUTE_ID_LOOPBACK: u32 = u32::MAX;
pub const NETMGR_OWNER_ID_SYSTEM: u64 = u64::MAX;
pub const NETMGR_SYSTEM_GENERATION: u32 = 1;
pub const NETMGR_IPV4_LOOPBACK: u32 = u32::from_be_bytes([127, 0, 0, 1]);
pub const NETMGR_IPV4_LOOPBACK_PREFIX: u8 = 8;

pub const NETMGR_RESPONSE_F_DEVICE: u8 = 1 << 0;
pub const NETMGR_RESPONSE_F_ADDRESS: u8 = 1 << 1;
pub const NETMGR_RESPONSE_F_ROUTE: u8 = 1 << 2;
const NETMGR_RESPONSE_F_ALL: u8 =
    NETMGR_RESPONSE_F_DEVICE | NETMGR_RESPONSE_F_ADDRESS | NETMGR_RESPONSE_F_ROUTE;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetmgrStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    NotFound = 3,
    AlreadyExists = 4,
    TableFull = 5,
    InvalidState = 6,
    InvalidPrefix = 7,
    LinkDown = 8,
    OwnerMismatch = 9,
    StaleGeneration = 10,
}

impl NetmgrStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::NotFound),
            4 => Some(Self::AlreadyExists),
            5 => Some(Self::TableFull),
            6 => Some(Self::InvalidState),
            7 => Some(Self::InvalidPrefix),
            8 => Some(Self::LinkDown),
            9 => Some(Self::OwnerMismatch),
            10 => Some(Self::StaleGeneration),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetmgrCodecError {
    Malformed,
    UnsupportedOpcode,
    InvalidPrefix,
    InvalidDevice,
    InvalidRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetDevice {
    pub device_id: u32,
    pub owner_id: u64,
    pub generation: u32,
    pub mac: [u8; 6],
    pub mtu: u16,
    pub flags: u32,
    pub link_up: bool,
}

impl NetDevice {
    pub const fn is_valid(self) -> bool {
        self.device_id != 0
            && self.owner_id != 0
            && self.generation != 0
            && self.mtu >= 576
            && self.mtu <= 9_000
            && self.flags & !NET_DEVICE_FLAG_ALL == 0
            && valid_unicast_mac(self.mac)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Address {
    pub device_id: u32,
    pub address: u32,
    pub prefix_len: u8,
    pub generation: u32,
    pub owner_id: u64,
}

impl Ipv4Address {
    pub const fn is_valid(self) -> bool {
        self.device_id != 0
            && self.address != 0
            && self.prefix_len <= 32
            && self.generation != 0
            && self.owner_id != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Route {
    pub route_id: u32,
    pub destination: u32,
    pub prefix_len: u8,
    pub gateway: u32,
    pub device_id: u32,
    pub metric: u32,
    pub generation: u32,
    pub owner_id: u64,
}

impl Ipv4Route {
    pub const fn is_valid(self) -> bool {
        self.route_id != 0
            && self.device_id != 0
            && self.generation != 0
            && self.owner_id != 0
            && self.prefix_len <= 32
            && self.destination == mask_ipv4(self.destination, self.prefix_len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetmgrRequest {
    RegisterDevice {
        device: NetDevice,
    },
    UnregisterDevice {
        device_id: u32,
        owner_id: u64,
        generation: u32,
    },
    GetDevice {
        device_id: u32,
    },
    ListDevices {
        start_index: u16,
    },
    SetLinkState {
        device_id: u32,
        owner_id: u64,
        generation: u32,
        link_up: bool,
    },
    AddIpv4Address {
        address: Ipv4Address,
    },
    RemoveIpv4Address {
        address: Ipv4Address,
    },
    AddRoute {
        route: Ipv4Route,
    },
    RemoveRoute {
        route_id: u32,
        owner_id: u64,
        generation: u32,
    },
    LookupRoute {
        destination: u32,
    },
    GetStatus,
    GetFirstIpv4AddressForDevice {
        device_id: u32,
    },
    CheckIpv4AddressOnDevice {
        device_id: u32,
        address: u32,
    },
}

impl NetmgrRequest {
    pub const ENCODED_LEN: usize = NETMGR_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; Self::ENCODED_LEN]), NetmgrCodecError> {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let opcode = match self {
            Self::RegisterDevice { device } => {
                encode_device(device, &mut payload[0..32])?;
                NETMGR_OP_REGISTER_DEVICE
            }
            Self::UnregisterDevice {
                device_id,
                owner_id,
                generation,
            } => {
                encode_control(device_id, owner_id, generation, false, &mut payload)?;
                NETMGR_OP_UNREGISTER_DEVICE
            }
            Self::GetDevice { device_id } => {
                require_nonzero(device_id)?;
                write_u32(&mut payload, 96, device_id);
                NETMGR_OP_GET_DEVICE
            }
            Self::ListDevices { start_index } => {
                write_u16(&mut payload, 112, start_index);
                NETMGR_OP_LIST_DEVICES
            }
            Self::SetLinkState {
                device_id,
                owner_id,
                generation,
                link_up,
            } => {
                encode_control(device_id, owner_id, generation, link_up, &mut payload)?;
                NETMGR_OP_SET_LINK_STATE
            }
            Self::AddIpv4Address { address } => {
                encode_address(address, &mut payload[32..56])?;
                NETMGR_OP_ADD_IPV4_ADDR
            }
            Self::RemoveIpv4Address { address } => {
                encode_address(address, &mut payload[32..56])?;
                NETMGR_OP_REMOVE_IPV4_ADDR
            }
            Self::AddRoute { route } => {
                encode_route(route, &mut payload[56..96])?;
                NETMGR_OP_ADD_ROUTE
            }
            Self::RemoveRoute {
                route_id,
                owner_id,
                generation,
            } => {
                require_nonzero(route_id)?;
                require_nonzero_u64(owner_id)?;
                require_nonzero(generation)?;
                write_u32(&mut payload, 96, route_id);
                write_u64(&mut payload, 100, owner_id);
                write_u32(&mut payload, 108, generation);
                NETMGR_OP_REMOVE_ROUTE
            }
            Self::LookupRoute { destination } => {
                write_u32(&mut payload, 96, destination);
                NETMGR_OP_LOOKUP_ROUTE
            }
            Self::GetStatus => NETMGR_OP_GET_STATUS,
            Self::GetFirstIpv4AddressForDevice { device_id } => {
                require_nonzero(device_id)?;
                write_u32(&mut payload, 96, device_id);
                NETMGR_OP_GET_FIRST_IPV4_ADDR_FOR_DEVICE
            }
            Self::CheckIpv4AddressOnDevice { device_id, address } => {
                require_nonzero(device_id)?;
                require_nonzero(address)?;
                write_u32(&mut payload, 96, device_id);
                write_u32(&mut payload, 100, address);
                NETMGR_OP_CHECK_IPV4_ADDR_ON_DEVICE
            }
        };
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, NetmgrCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(NetmgrCodecError::Malformed);
        }
        match opcode {
            NETMGR_OP_REGISTER_DEVICE => {
                require_zero(&payload[32..])?;
                Ok(Self::RegisterDevice {
                    device: decode_device(&payload[0..32])?,
                })
            }
            NETMGR_OP_UNREGISTER_DEVICE | NETMGR_OP_SET_LINK_STATE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[113..])?;
                let device_id = read_nonzero_u32(payload, 96)?;
                let owner_id = read_nonzero_u64(payload, 100)?;
                let generation = read_nonzero_u32(payload, 108)?;
                let link_up = decode_bool(payload[112])?;
                if opcode == NETMGR_OP_UNREGISTER_DEVICE {
                    if link_up {
                        return Err(NetmgrCodecError::Malformed);
                    }
                    Ok(Self::UnregisterDevice {
                        device_id,
                        owner_id,
                        generation,
                    })
                } else {
                    Ok(Self::SetLinkState {
                        device_id,
                        owner_id,
                        generation,
                        link_up,
                    })
                }
            }
            NETMGR_OP_GET_DEVICE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[100..])?;
                Ok(Self::GetDevice {
                    device_id: read_nonzero_u32(payload, 96)?,
                })
            }
            NETMGR_OP_LIST_DEVICES => {
                require_zero(&payload[..112])?;
                require_zero(&payload[114..])?;
                Ok(Self::ListDevices {
                    start_index: read_u16(payload, 112)?,
                })
            }
            NETMGR_OP_ADD_IPV4_ADDR | NETMGR_OP_REMOVE_IPV4_ADDR => {
                require_zero(&payload[..32])?;
                require_zero(&payload[56..])?;
                let address = decode_address(&payload[32..56])?;
                Ok(if opcode == NETMGR_OP_ADD_IPV4_ADDR {
                    Self::AddIpv4Address { address }
                } else {
                    Self::RemoveIpv4Address { address }
                })
            }
            NETMGR_OP_ADD_ROUTE => {
                require_zero(&payload[..56])?;
                require_zero(&payload[96..])?;
                Ok(Self::AddRoute {
                    route: decode_route(&payload[56..96])?,
                })
            }
            NETMGR_OP_REMOVE_ROUTE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[112..])?;
                Ok(Self::RemoveRoute {
                    route_id: read_nonzero_u32(payload, 96)?,
                    owner_id: read_nonzero_u64(payload, 100)?,
                    generation: read_nonzero_u32(payload, 108)?,
                })
            }
            NETMGR_OP_LOOKUP_ROUTE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[100..])?;
                Ok(Self::LookupRoute {
                    destination: read_u32(payload, 96)?,
                })
            }
            NETMGR_OP_GET_STATUS => {
                require_zero(payload)?;
                Ok(Self::GetStatus)
            }
            NETMGR_OP_GET_FIRST_IPV4_ADDR_FOR_DEVICE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[100..])?;
                Ok(Self::GetFirstIpv4AddressForDevice {
                    device_id: read_nonzero_u32(payload, 96)?,
                })
            }
            NETMGR_OP_CHECK_IPV4_ADDR_ON_DEVICE => {
                require_zero(&payload[..96])?;
                require_zero(&payload[104..])?;
                Ok(Self::CheckIpv4AddressOnDevice {
                    device_id: read_nonzero_u32(payload, 96)?,
                    address: read_nonzero_u32(payload, 100)?,
                })
            }
            _ => Err(NetmgrCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrResponse {
    pub status: NetmgrStatus,
    pub device: Option<NetDevice>,
    pub address: Option<Ipv4Address>,
    pub route: Option<Ipv4Route>,
    pub value: u32,
    pub auxiliary: u32,
}

impl NetmgrResponse {
    pub const ENCODED_LEN: usize = NETMGR_WIRE_LEN;

    pub const fn status(status: NetmgrStatus) -> Self {
        Self {
            status,
            device: None,
            address: None,
            route: None,
            value: 0,
            auxiliary: 0,
        }
    }

    pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], NetmgrCodecError> {
        let mut payload = [0u8; Self::ENCODED_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        write_u32(&mut payload, 8, self.value);
        write_u32(&mut payload, 12, self.auxiliary);
        if let Some(device) = self.device {
            payload[4] |= NETMGR_RESPONSE_F_DEVICE;
            encode_device(device, &mut payload[16..48])?;
        }
        if let Some(address) = self.address {
            payload[4] |= NETMGR_RESPONSE_F_ADDRESS;
            encode_address(address, &mut payload[48..72])?;
        }
        if let Some(route) = self.route {
            payload[4] |= NETMGR_RESPONSE_F_ROUTE;
            encode_route(route, &mut payload[72..112])?;
        }
        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, NetmgrCodecError> {
        if payload.len() != Self::ENCODED_LEN
            || payload[4] & !NETMGR_RESPONSE_F_ALL != 0
            || payload[5..8].iter().any(|byte| *byte != 0)
            || payload[112..].iter().any(|byte| *byte != 0)
        {
            return Err(NetmgrCodecError::Malformed);
        }
        let flags = payload[4];
        let device = if flags & NETMGR_RESPONSE_F_DEVICE != 0 {
            Some(decode_device(&payload[16..48])?)
        } else {
            require_zero(&payload[16..48])?;
            None
        };
        let address = if flags & NETMGR_RESPONSE_F_ADDRESS != 0 {
            Some(decode_address(&payload[48..72])?)
        } else {
            require_zero(&payload[48..72])?;
            None
        };
        let route = if flags & NETMGR_RESPONSE_F_ROUTE != 0 {
            Some(decode_route(&payload[72..112])?)
        } else {
            require_zero(&payload[72..112])?;
            None
        };
        Ok(Self {
            status: NetmgrStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(NetmgrCodecError::Malformed)?,
            device,
            address,
            route,
            value: read_u32(payload, 8)?,
            auxiliary: read_u32(payload, 12)?,
        })
    }
}

pub const fn mask_ipv4(address: u32, prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else if prefix_len <= 32 {
        address & (u32::MAX << (32 - prefix_len as u32))
    } else {
        address
    }
}

pub const fn ipv4_prefix_matches(address: u32, destination: u32, prefix_len: u8) -> bool {
    prefix_len <= 32 && mask_ipv4(address, prefix_len) == mask_ipv4(destination, prefix_len)
}

const fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    let nonzero =
        mac[0] != 0 || mac[1] != 0 || mac[2] != 0 || mac[3] != 0 || mac[4] != 0 || mac[5] != 0;
    nonzero && mac[0] & 1 == 0
}

fn encode_device(device: NetDevice, payload: &mut [u8]) -> Result<(), NetmgrCodecError> {
    if payload.len() != 32 || !device.is_valid() {
        return Err(NetmgrCodecError::InvalidDevice);
    }
    write_u32(payload, 0, device.device_id);
    write_u64(payload, 4, device.owner_id);
    write_u32(payload, 12, device.generation);
    write_u32(payload, 16, device.flags);
    write_u16(payload, 20, device.mtu);
    payload[22] = u8::from(device.link_up);
    payload[23..29].copy_from_slice(&device.mac);
    Ok(())
}

fn decode_device(payload: &[u8]) -> Result<NetDevice, NetmgrCodecError> {
    if payload.len() != 32 || payload[29..32].iter().any(|byte| *byte != 0) {
        return Err(NetmgrCodecError::Malformed);
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&payload[23..29]);
    let device = NetDevice {
        device_id: read_u32(payload, 0)?,
        owner_id: read_u64(payload, 4)?,
        generation: read_u32(payload, 12)?,
        flags: read_u32(payload, 16)?,
        mtu: read_u16(payload, 20)?,
        link_up: decode_bool(payload[22])?,
        mac,
    };
    if device.is_valid() {
        Ok(device)
    } else {
        Err(NetmgrCodecError::InvalidDevice)
    }
}

fn encode_address(address: Ipv4Address, payload: &mut [u8]) -> Result<(), NetmgrCodecError> {
    if payload.len() != 24 {
        return Err(NetmgrCodecError::Malformed);
    }
    if address.prefix_len > 32 {
        return Err(NetmgrCodecError::InvalidPrefix);
    }
    if !address.is_valid() {
        return Err(NetmgrCodecError::Malformed);
    }
    write_u32(payload, 0, address.device_id);
    write_u32(payload, 4, address.address);
    payload[8] = address.prefix_len;
    write_u32(payload, 12, address.generation);
    write_u64(payload, 16, address.owner_id);
    Ok(())
}

fn decode_address(payload: &[u8]) -> Result<Ipv4Address, NetmgrCodecError> {
    if payload.len() != 24 || payload[9..12].iter().any(|byte| *byte != 0) {
        return Err(NetmgrCodecError::Malformed);
    }
    let prefix_len = payload[8];
    if prefix_len > 32 {
        return Err(NetmgrCodecError::InvalidPrefix);
    }
    let address = Ipv4Address {
        device_id: read_u32(payload, 0)?,
        address: read_u32(payload, 4)?,
        prefix_len,
        generation: read_u32(payload, 12)?,
        owner_id: read_u64(payload, 16)?,
    };
    if address.is_valid() {
        Ok(address)
    } else {
        Err(NetmgrCodecError::Malformed)
    }
}

fn encode_route(route: Ipv4Route, payload: &mut [u8]) -> Result<(), NetmgrCodecError> {
    if payload.len() != 40 {
        return Err(NetmgrCodecError::Malformed);
    }
    if route.prefix_len > 32 {
        return Err(NetmgrCodecError::InvalidPrefix);
    }
    if !route.is_valid() {
        return Err(NetmgrCodecError::InvalidRoute);
    }
    write_u32(payload, 0, route.route_id);
    write_u32(payload, 4, route.destination);
    write_u32(payload, 8, route.gateway);
    write_u32(payload, 12, route.device_id);
    write_u32(payload, 16, route.metric);
    write_u32(payload, 20, route.generation);
    write_u64(payload, 24, route.owner_id);
    payload[32] = route.prefix_len;
    Ok(())
}

fn decode_route(payload: &[u8]) -> Result<Ipv4Route, NetmgrCodecError> {
    if payload.len() != 40 || payload[33..40].iter().any(|byte| *byte != 0) {
        return Err(NetmgrCodecError::Malformed);
    }
    let prefix_len = payload[32];
    if prefix_len > 32 {
        return Err(NetmgrCodecError::InvalidPrefix);
    }
    let route = Ipv4Route {
        route_id: read_u32(payload, 0)?,
        destination: read_u32(payload, 4)?,
        gateway: read_u32(payload, 8)?,
        device_id: read_u32(payload, 12)?,
        metric: read_u32(payload, 16)?,
        generation: read_u32(payload, 20)?,
        owner_id: read_u64(payload, 24)?,
        prefix_len,
    };
    if route.is_valid() {
        Ok(route)
    } else {
        Err(NetmgrCodecError::InvalidRoute)
    }
}

fn encode_control(
    device_id: u32,
    owner_id: u64,
    generation: u32,
    link_up: bool,
    payload: &mut [u8],
) -> Result<(), NetmgrCodecError> {
    require_nonzero(device_id)?;
    require_nonzero_u64(owner_id)?;
    require_nonzero(generation)?;
    write_u32(payload, 96, device_id);
    write_u64(payload, 100, owner_id);
    write_u32(payload, 108, generation);
    payload[112] = u8::from(link_up);
    Ok(())
}

fn require_nonzero(value: u32) -> Result<(), NetmgrCodecError> {
    if value == 0 {
        Err(NetmgrCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn require_nonzero_u64(value: u64) -> Result<(), NetmgrCodecError> {
    if value == 0 {
        Err(NetmgrCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn require_zero(payload: &[u8]) -> Result<(), NetmgrCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(NetmgrCodecError::Malformed)
    } else {
        Ok(())
    }
}

fn decode_bool(value: u8) -> Result<bool, NetmgrCodecError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(NetmgrCodecError::Malformed),
    }
}

fn read_nonzero_u32(payload: &[u8], offset: usize) -> Result<u32, NetmgrCodecError> {
    let value = read_u32(payload, offset)?;
    require_nonzero(value)?;
    Ok(value)
}

fn read_nonzero_u64(payload: &[u8], offset: usize) -> Result<u64, NetmgrCodecError> {
    let value = read_u64(payload, offset)?;
    require_nonzero_u64(value)?;
    Ok(value)
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

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, NetmgrCodecError> {
    let bytes = payload
        .get(offset..offset + 2)
        .ok_or(NetmgrCodecError::Malformed)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, NetmgrCodecError> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or(NetmgrCodecError::Malformed)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, NetmgrCodecError> {
    let bytes = payload
        .get(offset..offset + 8)
        .ok_or(NetmgrCodecError::Malformed)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> NetDevice {
        NetDevice {
            device_id: 7,
            owner_id: 11,
            generation: 2,
            mac: [0x02, 0, 0, 0, 0, 7],
            mtu: 1500,
            flags: NET_DEVICE_FLAG_BROADCAST,
            link_up: true,
        }
    }

    fn address() -> Ipv4Address {
        Ipv4Address {
            device_id: 7,
            address: u32::from_be_bytes([10, 0, 0, 2]),
            prefix_len: 24,
            generation: 2,
            owner_id: 11,
        }
    }

    fn route() -> Ipv4Route {
        Ipv4Route {
            route_id: 9,
            destination: u32::from_be_bytes([10, 0, 0, 0]),
            prefix_len: 24,
            gateway: 0,
            device_id: 7,
            metric: 10,
            generation: 2,
            owner_id: 11,
        }
    }

    #[test]
    fn netmgr_request_roundtrips() {
        let requests = [
            NetmgrRequest::RegisterDevice { device: device() },
            NetmgrRequest::UnregisterDevice {
                device_id: 7,
                owner_id: 11,
                generation: 2,
            },
            NetmgrRequest::GetDevice { device_id: 7 },
            NetmgrRequest::ListDevices { start_index: 3 },
            NetmgrRequest::SetLinkState {
                device_id: 7,
                owner_id: 11,
                generation: 2,
                link_up: false,
            },
            NetmgrRequest::AddIpv4Address { address: address() },
            NetmgrRequest::RemoveIpv4Address { address: address() },
            NetmgrRequest::AddRoute { route: route() },
            NetmgrRequest::RemoveRoute {
                route_id: 9,
                owner_id: 11,
                generation: 2,
            },
            NetmgrRequest::LookupRoute {
                destination: u32::from_be_bytes([10, 0, 0, 8]),
            },
            NetmgrRequest::GetStatus,
            NetmgrRequest::GetFirstIpv4AddressForDevice { device_id: 7 },
            NetmgrRequest::CheckIpv4AddressOnDevice {
                device_id: 7,
                address: u32::from_be_bytes([10, 0, 0, 2]),
            },
        ];
        for request in requests {
            let (opcode, encoded) = request.encode().expect("encode request");
            assert_eq!(NetmgrRequest::decode(opcode, &encoded), Ok(request));
        }
    }

    #[test]
    fn netmgr_response_roundtrips() {
        let response = NetmgrResponse {
            status: NetmgrStatus::Ok,
            device: Some(device()),
            address: Some(address()),
            route: Some(route()),
            value: 3,
            auxiliary: 4,
        };
        let encoded = response.encode().expect("encode response");
        assert_eq!(NetmgrResponse::decode(&encoded), Ok(response));
    }

    #[test]
    fn netmgr_rejects_nonzero_reserved_fields() {
        let (opcode, mut encoded) = NetmgrRequest::GetStatus.encode().expect("encode");
        encoded[127] = 1;
        assert_eq!(
            NetmgrRequest::decode(opcode, &encoded),
            Err(NetmgrCodecError::Malformed)
        );
    }

    #[test]
    fn netmgr_address_queries_reject_invalid_ids_and_reserved_bytes() {
        assert_eq!(
            NetmgrRequest::GetFirstIpv4AddressForDevice { device_id: 0 }.encode(),
            Err(NetmgrCodecError::Malformed)
        );
        assert_eq!(
            NetmgrRequest::CheckIpv4AddressOnDevice {
                device_id: 7,
                address: 0,
            }
            .encode(),
            Err(NetmgrCodecError::Malformed)
        );
        let (opcode, mut payload) = NetmgrRequest::CheckIpv4AddressOnDevice {
            device_id: 7,
            address: u32::from_be_bytes([10, 0, 0, 2]),
        }
        .encode()
        .expect("encode query");
        payload[104] = 1;
        assert_eq!(
            NetmgrRequest::decode(opcode, &payload),
            Err(NetmgrCodecError::Malformed)
        );
    }

    #[test]
    fn netmgr_rejects_unknown_opcode() {
        assert_eq!(
            NetmgrRequest::decode(0xffff, &[0; NETMGR_WIRE_LEN]),
            Err(NetmgrCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn netmgr_rejects_invalid_prefix() {
        let mut invalid = route();
        invalid.prefix_len = 33;
        assert_eq!(
            NetmgrRequest::AddRoute { route: invalid }.encode(),
            Err(NetmgrCodecError::InvalidPrefix)
        );
    }

    #[test]
    fn netmgr_constants_and_statuses_are_stable() {
        assert_eq!(NETMGR_ABI_VERSION, 1);
        assert_eq!(NETMGR_WIRE_LEN, 128);
        assert_eq!(NETMGR_OP_REGISTER_DEVICE, 1);
        assert_eq!(NETMGR_OP_GET_STATUS, 11);
        assert_eq!(NETMGR_OP_GET_FIRST_IPV4_ADDR_FOR_DEVICE, 12);
        assert_eq!(NETMGR_OP_CHECK_IPV4_ADDR_ON_DEVICE, 13);
        assert_eq!(NETMGR_DEVICE_ID_LOOPBACK, 1);
        assert_eq!(NETMGR_IPV4_LOOPBACK, u32::from_be_bytes([127, 0, 0, 1]));
        assert_eq!(NETMGR_IPV4_LOOPBACK_PREFIX, 8);
        assert_eq!(NetmgrStatus::Ok as u32, 0);
        assert_eq!(NetmgrStatus::LinkDown as u32, 8);
        assert_eq!(NetmgrStatus::StaleGeneration as u32, 10);
    }
}
