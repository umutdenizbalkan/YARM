// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Generic userspace NIC-driver service protocol.
//!
//! Version 1 provides bounded inline test packets and device metadata. It does
//! not define DMA, hardware virtqueues, interrupts, or kernel syscalls.

pub const NETDEV_ABI_VERSION: u16 = 1;
pub const NETDEV_WIRE_LEN: usize = 128;
pub const NETDEV_MAX_INLINE_PACKET: usize = 96;

pub const NETDEV_OP_GET_INFO: u16 = 1;
pub const NETDEV_OP_GET_STATUS: u16 = 2;
pub const NETDEV_OP_SET_LINK_STATE: u16 = 3;
pub const NETDEV_OP_TX_INLINE: u16 = 4;
pub const NETDEV_OP_RX_INLINE: u16 = 5;
pub const NETDEV_OP_INJECT_RX_TEST: u16 = 6;
pub const NETDEV_OP_DRAIN_TX_TEST: u16 = 7;
pub const NETDEV_OP_CLEAR_STATS: u16 = 8;

pub const NETDEV_FEATURE_FAKE_TX: u32 = 1 << 0;
pub const NETDEV_FEATURE_FAKE_RX: u32 = 1 << 1;
pub const NETDEV_FEATURE_TEST_CONTROL: u32 = 1 << 2;
pub const NETDEV_FEATURE_ALL: u32 =
    NETDEV_FEATURE_FAKE_TX | NETDEV_FEATURE_FAKE_RX | NETDEV_FEATURE_TEST_CONTROL;

pub const NETDEV_PACKET_F_NONE: u16 = 0;
pub const NETDEV_PACKET_F_CHECKSUM_REQUESTED: u16 = 1 << 0;
pub const NETDEV_PACKET_F_ALL: u16 = NETDEV_PACKET_F_CHECKSUM_REQUESTED;

pub const NETDEV_ETHERTYPE_NONE: u16 = 0;
pub const NETDEV_ETHERTYPE_IPV4: u16 = 0x0800;
pub const NETDEV_ETHERTYPE_ARP: u16 = 0x0806;
pub const NETDEV_ETHERTYPE_IPV6: u16 = 0x86dd;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetdevStatus {
    Ok = 0,
    BadRequest = 1,
    Unsupported = 2,
    TableFull = 3,
    Empty = 4,
    MessageTooLarge = 5,
    LinkDown = 6,
    InvalidState = 7,
    ChecksumUnsupported = 8,
}

impl NetdevStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::Unsupported),
            3 => Some(Self::TableFull),
            4 => Some(Self::Empty),
            5 => Some(Self::MessageTooLarge),
            6 => Some(Self::LinkDown),
            7 => Some(Self::InvalidState),
            8 => Some(Self::ChecksumUnsupported),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetdevCodecError {
    Malformed,
    UnsupportedOpcode,
    InvalidDevice,
    InvalidPacket,
    MessageTooLarge,
    UnsupportedFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetdevInfo {
    pub device_id: u32,
    pub generation: u32,
    pub mac: [u8; 6],
    pub mtu: u16,
    pub features: u32,
    pub tx_capacity: u16,
    pub rx_capacity: u16,
    pub link_up: bool,
}

impl NetdevInfo {
    pub const fn is_valid(self) -> bool {
        self.device_id != 0
            && self.generation != 0
            && valid_unicast_mac(self.mac)
            && self.mtu >= 576
            && self.mtu <= 9_000
            && self.features & !NETDEV_FEATURE_ALL == 0
            && self.tx_capacity != 0
            && self.rx_capacity != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetdevPacket {
    pub packet_id: u32,
    pub len: u16,
    pub flags: u16,
    pub ethertype: u16,
    pub data: [u8; NETDEV_MAX_INLINE_PACKET],
}

impl NetdevPacket {
    pub fn new(
        packet_id: u32,
        flags: u16,
        ethertype: u16,
        bytes: &[u8],
    ) -> Result<Self, NetdevCodecError> {
        if bytes.is_empty() || bytes.len() > NETDEV_MAX_INLINE_PACKET {
            return Err(NetdevCodecError::MessageTooLarge);
        }
        let mut data = [0; NETDEV_MAX_INLINE_PACKET];
        data[..bytes.len()].copy_from_slice(bytes);
        let packet = Self {
            packet_id,
            len: bytes.len() as u16,
            flags,
            ethertype,
            data,
        };
        validate_packet(packet)?;
        Ok(packet)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetdevRequest {
    GetInfo,
    GetStatus,
    SetLinkState { link_up: bool },
    TxInline { packet: NetdevPacket },
    RxInline,
    InjectRxTest { packet: NetdevPacket },
    DrainTxTest,
    ClearStats,
}

impl NetdevRequest {
    pub const ENCODED_LEN: usize = NETDEV_WIRE_LEN;

    pub fn encode(self) -> Result<(u16, [u8; Self::ENCODED_LEN]), NetdevCodecError> {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let opcode = match self {
            Self::GetInfo => NETDEV_OP_GET_INFO,
            Self::GetStatus => NETDEV_OP_GET_STATUS,
            Self::SetLinkState { link_up } => {
                payload[0] = u8::from(link_up);
                NETDEV_OP_SET_LINK_STATE
            }
            Self::TxInline { packet } => {
                encode_packet(packet, &mut payload)?;
                NETDEV_OP_TX_INLINE
            }
            Self::RxInline => NETDEV_OP_RX_INLINE,
            Self::InjectRxTest { packet } => {
                encode_packet(packet, &mut payload)?;
                NETDEV_OP_INJECT_RX_TEST
            }
            Self::DrainTxTest => NETDEV_OP_DRAIN_TX_TEST,
            Self::ClearStats => NETDEV_OP_CLEAR_STATS,
        };
        Ok((opcode, payload))
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, NetdevCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(NetdevCodecError::Malformed);
        }
        match opcode {
            NETDEV_OP_GET_INFO
            | NETDEV_OP_GET_STATUS
            | NETDEV_OP_RX_INLINE
            | NETDEV_OP_DRAIN_TX_TEST
            | NETDEV_OP_CLEAR_STATS => {
                require_zero(payload)?;
                Ok(match opcode {
                    NETDEV_OP_GET_INFO => Self::GetInfo,
                    NETDEV_OP_GET_STATUS => Self::GetStatus,
                    NETDEV_OP_RX_INLINE => Self::RxInline,
                    NETDEV_OP_DRAIN_TX_TEST => Self::DrainTxTest,
                    _ => Self::ClearStats,
                })
            }
            NETDEV_OP_SET_LINK_STATE => {
                require_zero(&payload[1..])?;
                Ok(Self::SetLinkState {
                    link_up: decode_bool(payload[0])?,
                })
            }
            NETDEV_OP_TX_INLINE | NETDEV_OP_INJECT_RX_TEST => {
                let packet = decode_packet(payload)?;
                Ok(if opcode == NETDEV_OP_TX_INLINE {
                    Self::TxInline { packet }
                } else {
                    Self::InjectRxTest { packet }
                })
            }
            _ => Err(NetdevCodecError::UnsupportedOpcode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetdevResponse {
    pub status: NetdevStatus,
    pub info: Option<NetdevInfo>,
    pub packet: Option<NetdevPacket>,
    pub tx_depth: u16,
    pub rx_depth: u16,
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub tx_dropped: u64,
    pub rx_dropped: u64,
    pub bad_requests: u64,
}

impl NetdevResponse {
    pub const ENCODED_LEN: usize = NETDEV_WIRE_LEN;

    pub const fn status(status: NetdevStatus) -> Self {
        Self {
            status,
            info: None,
            packet: None,
            tx_depth: 0,
            rx_depth: 0,
            tx_packets: 0,
            rx_packets: 0,
            tx_dropped: 0,
            rx_dropped: 0,
            bad_requests: 0,
        }
    }

    pub fn encode(self) -> Result<[u8; Self::ENCODED_LEN], NetdevCodecError> {
        if self.info.is_some() && self.packet.is_some() {
            return Err(NetdevCodecError::Malformed);
        }
        let mut payload = [0u8; Self::ENCODED_LEN];
        write_u32(&mut payload, 0, self.status as u32);
        write_u16(&mut payload, 6, self.tx_depth);
        write_u16(&mut payload, 8, self.rx_depth);
        if let Some(packet) = self.packet {
            if self.tx_packets != 0
                || self.rx_packets != 0
                || self.tx_dropped != 0
                || self.rx_dropped != 0
                || self.bad_requests != 0
            {
                return Err(NetdevCodecError::Malformed);
            }
            payload[4] = 2;
            encode_packet(packet, &mut payload[16..])?;
            return Ok(payload);
        }
        write_u64(&mut payload, 16, self.tx_packets);
        write_u64(&mut payload, 24, self.rx_packets);
        write_u64(&mut payload, 32, self.tx_dropped);
        write_u64(&mut payload, 40, self.rx_dropped);
        write_u64(&mut payload, 48, self.bad_requests);
        if let Some(info) = self.info {
            payload[4] = 1;
            encode_info(info, &mut payload[64..96])?;
        }
        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, NetdevCodecError> {
        if payload.len() != Self::ENCODED_LEN
            || payload[5] != 0
            || payload[10..16].iter().any(|byte| *byte != 0)
        {
            return Err(NetdevCodecError::Malformed);
        }
        let variant = payload[4];
        let mut response = Self {
            status: NetdevStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(NetdevCodecError::Malformed)?,
            info: None,
            packet: None,
            tx_depth: read_u16(payload, 6)?,
            rx_depth: read_u16(payload, 8)?,
            tx_packets: read_u64(payload, 16)?,
            rx_packets: read_u64(payload, 24)?,
            tx_dropped: read_u64(payload, 32)?,
            rx_dropped: read_u64(payload, 40)?,
            bad_requests: read_u64(payload, 48)?,
        };
        match variant {
            0 => require_zero(&payload[56..])?,
            1 => {
                require_zero(&payload[56..64])?;
                response.info = Some(decode_info(&payload[64..96])?);
                require_zero(&payload[96..])?;
            }
            2 => {
                response.packet = Some(decode_packet(&payload[16..])?);
                response.tx_packets = 0;
                response.rx_packets = 0;
                response.tx_dropped = 0;
                response.rx_dropped = 0;
                response.bad_requests = 0;
            }
            _ => return Err(NetdevCodecError::Malformed),
        }
        Ok(response)
    }
}

fn validate_packet(packet: NetdevPacket) -> Result<(), NetdevCodecError> {
    if packet.packet_id == 0 || packet.len == 0 {
        return Err(NetdevCodecError::InvalidPacket);
    }
    if usize::from(packet.len) > NETDEV_MAX_INLINE_PACKET {
        return Err(NetdevCodecError::MessageTooLarge);
    }
    if packet.flags & !NETDEV_PACKET_F_ALL != 0 {
        return Err(NetdevCodecError::UnsupportedFlags);
    }
    if packet.data[usize::from(packet.len)..]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(NetdevCodecError::Malformed);
    }
    Ok(())
}

fn encode_packet(packet: NetdevPacket, payload: &mut [u8]) -> Result<(), NetdevCodecError> {
    if payload.len() != NETDEV_WIRE_LEN && payload.len() != NETDEV_WIRE_LEN - 16 {
        return Err(NetdevCodecError::Malformed);
    }
    validate_packet(packet)?;
    write_u32(payload, 0, packet.packet_id);
    write_u16(payload, 4, packet.len);
    write_u16(payload, 6, packet.flags);
    write_u16(payload, 8, packet.ethertype);
    payload[16..16 + usize::from(packet.len)]
        .copy_from_slice(&packet.data[..usize::from(packet.len)]);
    Ok(())
}

fn decode_packet(payload: &[u8]) -> Result<NetdevPacket, NetdevCodecError> {
    if (payload.len() != NETDEV_WIRE_LEN && payload.len() != NETDEV_WIRE_LEN - 16)
        || payload[10..16].iter().any(|byte| *byte != 0)
    {
        return Err(NetdevCodecError::Malformed);
    }
    let len = read_u16(payload, 4)?;
    if usize::from(len) > NETDEV_MAX_INLINE_PACKET {
        return Err(NetdevCodecError::MessageTooLarge);
    }
    let mut data = [0u8; NETDEV_MAX_INLINE_PACKET];
    data[..usize::from(len)].copy_from_slice(&payload[16..16 + usize::from(len)]);
    require_zero(&payload[16 + usize::from(len)..])?;
    let packet = NetdevPacket {
        packet_id: read_u32(payload, 0)?,
        len,
        flags: read_u16(payload, 6)?,
        ethertype: read_u16(payload, 8)?,
        data,
    };
    validate_packet(packet)?;
    Ok(packet)
}

fn encode_info(info: NetdevInfo, payload: &mut [u8]) -> Result<(), NetdevCodecError> {
    if payload.len() != 32 || !info.is_valid() {
        return Err(NetdevCodecError::InvalidDevice);
    }
    write_u32(payload, 0, info.device_id);
    write_u32(payload, 4, info.generation);
    payload[8..14].copy_from_slice(&info.mac);
    write_u16(payload, 14, info.mtu);
    write_u32(payload, 16, info.features);
    write_u16(payload, 20, info.tx_capacity);
    write_u16(payload, 22, info.rx_capacity);
    payload[24] = u8::from(info.link_up);
    Ok(())
}

fn decode_info(payload: &[u8]) -> Result<NetdevInfo, NetdevCodecError> {
    if payload.len() != 32 || payload[25..].iter().any(|byte| *byte != 0) {
        return Err(NetdevCodecError::Malformed);
    }
    let mut mac = [0; 6];
    mac.copy_from_slice(&payload[8..14]);
    let info = NetdevInfo {
        device_id: read_u32(payload, 0)?,
        generation: read_u32(payload, 4)?,
        mac,
        mtu: read_u16(payload, 14)?,
        features: read_u32(payload, 16)?,
        tx_capacity: read_u16(payload, 20)?,
        rx_capacity: read_u16(payload, 22)?,
        link_up: decode_bool(payload[24])?,
    };
    if info.is_valid() {
        Ok(info)
    } else {
        Err(NetdevCodecError::InvalidDevice)
    }
}

const fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    let nonzero =
        mac[0] != 0 || mac[1] != 0 || mac[2] != 0 || mac[3] != 0 || mac[4] != 0 || mac[5] != 0;
    nonzero && mac[0] & 1 == 0
}

fn decode_bool(value: u8) -> Result<bool, NetdevCodecError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(NetdevCodecError::Malformed),
    }
}

fn require_zero(payload: &[u8]) -> Result<(), NetdevCodecError> {
    if payload.iter().any(|byte| *byte != 0) {
        Err(NetdevCodecError::Malformed)
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

fn read_u16(payload: &[u8], offset: usize) -> Result<u16, NetdevCodecError> {
    let bytes = payload
        .get(offset..offset + 2)
        .ok_or(NetdevCodecError::Malformed)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, NetdevCodecError> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or(NetdevCodecError::Malformed)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, NetdevCodecError> {
    let bytes = payload
        .get(offset..offset + 8)
        .ok_or(NetdevCodecError::Malformed)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> NetdevPacket {
        NetdevPacket::new(7, NETDEV_PACKET_F_NONE, NETDEV_ETHERTYPE_IPV4, b"packet")
            .expect("packet")
    }

    fn info() -> NetdevInfo {
        NetdevInfo {
            device_id: 2,
            generation: 1,
            mac: [0x02, 0, 0, 0, 0, 2],
            mtu: 1500,
            features: NETDEV_FEATURE_ALL,
            tx_capacity: 8,
            rx_capacity: 8,
            link_up: true,
        }
    }

    #[test]
    fn netdev_request_roundtrips() {
        let requests = [
            NetdevRequest::GetInfo,
            NetdevRequest::GetStatus,
            NetdevRequest::SetLinkState { link_up: false },
            NetdevRequest::TxInline { packet: packet() },
            NetdevRequest::RxInline,
            NetdevRequest::InjectRxTest { packet: packet() },
            NetdevRequest::DrainTxTest,
            NetdevRequest::ClearStats,
        ];
        for request in requests {
            let (opcode, encoded) = request.encode().expect("encode");
            assert_eq!(NetdevRequest::decode(opcode, &encoded), Ok(request));
        }
    }

    #[test]
    fn netdev_response_roundtrips_info_packet_and_status() {
        let responses = [
            NetdevResponse {
                info: Some(info()),
                ..NetdevResponse::status(NetdevStatus::Ok)
            },
            NetdevResponse {
                packet: Some(packet()),
                tx_depth: 1,
                ..NetdevResponse::status(NetdevStatus::Ok)
            },
            NetdevResponse {
                tx_packets: 4,
                rx_packets: 3,
                tx_dropped: 2,
                rx_dropped: 1,
                bad_requests: 5,
                ..NetdevResponse::status(NetdevStatus::Ok)
            },
        ];
        for response in responses {
            assert_eq!(
                NetdevResponse::decode(&response.encode().expect("encode")),
                Ok(response)
            );
        }
    }

    #[test]
    fn netdev_rejects_reserved_fields_and_unknown_opcode() {
        let mut payload = [0u8; NETDEV_WIRE_LEN];
        payload[127] = 1;
        assert_eq!(
            NetdevRequest::decode(NETDEV_OP_GET_INFO, &payload),
            Err(NetdevCodecError::Malformed)
        );
        assert_eq!(
            NetdevRequest::decode(0xffff, &[0; NETDEV_WIRE_LEN]),
            Err(NetdevCodecError::UnsupportedOpcode)
        );
    }

    #[test]
    fn netdev_rejects_invalid_info_and_packet_lengths() {
        let mut invalid = info();
        invalid.mac = [0; 6];
        assert_eq!(
            NetdevResponse {
                info: Some(invalid),
                ..NetdevResponse::status(NetdevStatus::Ok)
            }
            .encode(),
            Err(NetdevCodecError::InvalidDevice)
        );
        let mut invalid_mtu = info();
        invalid_mtu.mtu = 0;
        assert_eq!(
            NetdevResponse {
                info: Some(invalid_mtu),
                ..NetdevResponse::status(NetdevStatus::Ok)
            }
            .encode(),
            Err(NetdevCodecError::InvalidDevice)
        );
        assert_eq!(
            NetdevPacket::new(1, 0, 0, &[]),
            Err(NetdevCodecError::MessageTooLarge)
        );
        assert_eq!(
            NetdevPacket::new(1, 0, 0, &[0; NETDEV_MAX_INLINE_PACKET + 1]),
            Err(NetdevCodecError::MessageTooLarge)
        );
    }

    #[test]
    fn netdev_constants_and_statuses_are_stable() {
        assert_eq!(NETDEV_ABI_VERSION, 1);
        assert_eq!(NETDEV_WIRE_LEN, 128);
        assert_eq!(NETDEV_MAX_INLINE_PACKET, 96);
        assert_eq!(NETDEV_OP_GET_INFO, 1);
        assert_eq!(NETDEV_OP_CLEAR_STATS, 8);
        assert_eq!(NetdevStatus::Ok as u32, 0);
        assert_eq!(NetdevStatus::ChecksumUnsupported as u32, 8);
    }
}
