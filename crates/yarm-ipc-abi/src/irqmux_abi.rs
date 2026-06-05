// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace IRQ multiplexer service protocol.
//!
//! Grant IDs and driver IDs in this ABI are opaque userspace authorization
//! identifiers. They are not kernel capabilities and do not prove possession
//! of a kernel IRQ capability.

pub const IRQMUX_ABI_VERSION: u16 = 2;

pub const IRQMUX_OP_REGISTER_LINE: u16 = 1;
pub const IRQMUX_OP_UNREGISTER_LINE: u16 = 2;
pub const IRQMUX_OP_BIND_DRIVER: u16 = 3;
pub const IRQMUX_OP_UNBIND_DRIVER: u16 = 4;
pub const IRQMUX_OP_ENABLE: u16 = 5;
pub const IRQMUX_OP_DISABLE: u16 = 6;
pub const IRQMUX_OP_MASK: u16 = 7;
pub const IRQMUX_OP_UNMASK: u16 = 8;
pub const IRQMUX_OP_ACK: u16 = 9;
pub const IRQMUX_OP_INJECT_TEST_IRQ: u16 = 10;
pub const IRQMUX_OP_GET_STATUS: u16 = 11;
pub const IRQMUX_OP_AUTHORIZE_GRANT: u16 = 12;
pub const IRQMUX_OP_REVOKE_GRANT: u16 = 13;

pub type IrqLine = u32;
pub type IrqVector = u32;
pub type IrqRouteTarget = u64;
pub type IrqGrantId = u64;
pub type IrqDriverId = u64;
pub type IrqGrantGeneration = u64;

pub const IRQ_GRANT_RIGHT_REGISTER: u32 = 1 << 0;
pub const IRQ_GRANT_RIGHT_BIND: u32 = 1 << 1;
pub const IRQ_GRANT_RIGHT_ENABLE: u32 = 1 << 2;
pub const IRQ_GRANT_RIGHT_MASK: u32 = 1 << 3;
pub const IRQ_GRANT_RIGHT_ACK: u32 = 1 << 4;
pub const IRQ_GRANT_RIGHT_ALL: u32 = IRQ_GRANT_RIGHT_REGISTER
    | IRQ_GRANT_RIGHT_BIND
    | IRQ_GRANT_RIGHT_ENABLE
    | IRQ_GRANT_RIGHT_MASK
    | IRQ_GRANT_RIGHT_ACK;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqTriggerMode {
    Edge = 1,
    Level = 2,
}

impl IrqTriggerMode {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Edge),
            2 => Some(Self::Level),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqPolarity {
    High = 1,
    Low = 2,
}

impl IrqPolarity {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::High),
            2 => Some(Self::Low),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMuxStatus {
    Ok = 0,
    NotFound = 1,
    AlreadyRegistered = 2,
    Busy = 3,
    Masked = 4,
    Disabled = 5,
    BadRequest = 6,
    Unsupported = 7,
    Unauthorized = 8,
    GrantNotFound = 9,
    GrantStale = 10,
    GrantMismatch = 11,
    RightsMissing = 12,
}

impl IrqMuxStatus {
    const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::NotFound),
            2 => Some(Self::AlreadyRegistered),
            3 => Some(Self::Busy),
            4 => Some(Self::Masked),
            5 => Some(Self::Disabled),
            6 => Some(Self::BadRequest),
            7 => Some(Self::Unsupported),
            8 => Some(Self::Unauthorized),
            9 => Some(Self::GrantNotFound),
            10 => Some(Self::GrantStale),
            11 => Some(Self::GrantMismatch),
            12 => Some(Self::RightsMissing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMuxCodecError {
    Malformed,
    UnknownRights,
    UnsupportedOpcode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqGrantKey {
    pub grant_id: IrqGrantId,
    pub driver_id: IrqDriverId,
    pub generation: IrqGrantGeneration,
}

impl IrqGrantKey {
    pub const fn new(
        grant_id: IrqGrantId,
        driver_id: IrqDriverId,
        generation: IrqGrantGeneration,
    ) -> Self {
        Self {
            grant_id,
            driver_id,
            generation,
        }
    }

    pub const fn is_valid(self) -> bool {
        self.grant_id != 0 && self.driver_id != 0 && self.generation != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqGrantDescriptor {
    pub key: IrqGrantKey,
    pub irq_line: IrqLine,
    pub irq_vector: IrqVector,
    pub rights: u32,
    pub trigger: IrqTriggerMode,
    pub polarity: IrqPolarity,
}

impl IrqGrantDescriptor {
    pub const ENCODED_LEN: usize = 48;

    pub const fn is_valid(self) -> bool {
        self.key.is_valid() && self.rights != 0 && self.rights & !IRQ_GRANT_RIGHT_ALL == 0
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        encode_key(self.key, &mut payload);
        payload[24..28].copy_from_slice(&self.irq_line.to_le_bytes());
        payload[28..32].copy_from_slice(&self.irq_vector.to_le_bytes());
        payload[32..36].copy_from_slice(&self.rights.to_le_bytes());
        payload[36] = self.trigger as u8;
        payload[37] = self.polarity as u8;
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, IrqMuxCodecError> {
        if payload.len() != Self::ENCODED_LEN || payload[38..48].iter().any(|byte| *byte != 0) {
            return Err(IrqMuxCodecError::Malformed);
        }
        let descriptor = Self {
            key: decode_key(payload)?,
            irq_line: read_u32(payload, 24)?,
            irq_vector: read_u32(payload, 28)?,
            rights: read_rights(payload, 32)?,
            trigger: IrqTriggerMode::from_wire(payload[36]).ok_or(IrqMuxCodecError::Malformed)?,
            polarity: IrqPolarity::from_wire(payload[37]).ok_or(IrqMuxCodecError::Malformed)?,
        };
        if descriptor.is_valid() {
            Ok(descriptor)
        } else {
            Err(IrqMuxCodecError::Malformed)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMuxRequest {
    AuthorizeGrant {
        grant: IrqGrantDescriptor,
    },
    RevokeGrant {
        key: IrqGrantKey,
    },
    RegisterLine {
        key: IrqGrantKey,
        line: IrqLine,
        vector: IrqVector,
        trigger: IrqTriggerMode,
        polarity: IrqPolarity,
    },
    UnregisterLine {
        key: IrqGrantKey,
        line: IrqLine,
    },
    BindDriver {
        key: IrqGrantKey,
        line: IrqLine,
        target: IrqRouteTarget,
    },
    UnbindDriver {
        key: IrqGrantKey,
        line: IrqLine,
    },
    Enable {
        key: IrqGrantKey,
        line: IrqLine,
    },
    Disable {
        key: IrqGrantKey,
        line: IrqLine,
    },
    Mask {
        key: IrqGrantKey,
        line: IrqLine,
    },
    Unmask {
        key: IrqGrantKey,
        line: IrqLine,
    },
    Ack {
        key: IrqGrantKey,
        line: IrqLine,
    },
    InjectTestIrq {
        line: IrqLine,
    },
    GetStatus {
        line: IrqLine,
    },
}

impl IrqMuxRequest {
    pub const ENCODED_LEN: usize = 64;

    pub const fn line(self) -> IrqLine {
        match self {
            Self::AuthorizeGrant { grant } => grant.irq_line,
            Self::RevokeGrant { .. } => 0,
            Self::RegisterLine { line, .. }
            | Self::UnregisterLine { line, .. }
            | Self::BindDriver { line, .. }
            | Self::UnbindDriver { line, .. }
            | Self::Enable { line, .. }
            | Self::Disable { line, .. }
            | Self::Mask { line, .. }
            | Self::Unmask { line, .. }
            | Self::Ack { line, .. }
            | Self::InjectTestIrq { line }
            | Self::GetStatus { line } => line,
        }
    }

    pub fn encode(self) -> (u16, [u8; Self::ENCODED_LEN]) {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let opcode = match self {
            Self::AuthorizeGrant { grant } => {
                payload[..IrqGrantDescriptor::ENCODED_LEN].copy_from_slice(&grant.encode());
                IRQMUX_OP_AUTHORIZE_GRANT
            }
            Self::RevokeGrant { key } => {
                encode_key(key, &mut payload);
                IRQMUX_OP_REVOKE_GRANT
            }
            Self::RegisterLine {
                key,
                line,
                vector,
                trigger,
                polarity,
            } => {
                encode_key_line(key, line, &mut payload);
                payload[28..32].copy_from_slice(&vector.to_le_bytes());
                payload[40] = trigger as u8;
                payload[41] = polarity as u8;
                IRQMUX_OP_REGISTER_LINE
            }
            Self::UnregisterLine { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_UNREGISTER_LINE
            }
            Self::BindDriver { key, line, target } => {
                encode_key_line(key, line, &mut payload);
                payload[32..40].copy_from_slice(&target.to_le_bytes());
                IRQMUX_OP_BIND_DRIVER
            }
            Self::UnbindDriver { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_UNBIND_DRIVER
            }
            Self::Enable { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_ENABLE
            }
            Self::Disable { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_DISABLE
            }
            Self::Mask { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_MASK
            }
            Self::Unmask { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_UNMASK
            }
            Self::Ack { key, line } => {
                encode_key_line(key, line, &mut payload);
                IRQMUX_OP_ACK
            }
            Self::InjectTestIrq { line } => {
                payload[24..28].copy_from_slice(&line.to_le_bytes());
                IRQMUX_OP_INJECT_TEST_IRQ
            }
            Self::GetStatus { line } => {
                payload[24..28].copy_from_slice(&line.to_le_bytes());
                IRQMUX_OP_GET_STATUS
            }
        };
        (opcode, payload)
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, IrqMuxCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(IrqMuxCodecError::Malformed);
        }
        match opcode {
            IRQMUX_OP_AUTHORIZE_GRANT => {
                require_zero(&payload[IrqGrantDescriptor::ENCODED_LEN..])?;
                Ok(Self::AuthorizeGrant {
                    grant: IrqGrantDescriptor::decode(&payload[..IrqGrantDescriptor::ENCODED_LEN])?,
                })
            }
            IRQMUX_OP_REVOKE_GRANT => {
                require_zero(&payload[24..])?;
                Ok(Self::RevokeGrant {
                    key: decode_valid_key(payload)?,
                })
            }
            IRQMUX_OP_REGISTER_LINE => {
                require_zero(&payload[32..40])?;
                require_zero(&payload[42..])?;
                Ok(Self::RegisterLine {
                    key: decode_valid_key(payload)?,
                    line: read_u32(payload, 24)?,
                    vector: read_u32(payload, 28)?,
                    trigger: IrqTriggerMode::from_wire(payload[40])
                        .ok_or(IrqMuxCodecError::Malformed)?,
                    polarity: IrqPolarity::from_wire(payload[41])
                        .ok_or(IrqMuxCodecError::Malformed)?,
                })
            }
            IRQMUX_OP_BIND_DRIVER => {
                require_zero(&payload[40..])?;
                let target = read_u64(payload, 32)?;
                if target == 0 {
                    return Err(IrqMuxCodecError::Malformed);
                }
                Ok(Self::BindDriver {
                    key: decode_valid_key(payload)?,
                    line: read_u32(payload, 24)?,
                    target,
                })
            }
            IRQMUX_OP_UNREGISTER_LINE
            | IRQMUX_OP_UNBIND_DRIVER
            | IRQMUX_OP_ENABLE
            | IRQMUX_OP_DISABLE
            | IRQMUX_OP_MASK
            | IRQMUX_OP_UNMASK
            | IRQMUX_OP_ACK => {
                require_zero(&payload[28..])?;
                let key = decode_valid_key(payload)?;
                let line = read_u32(payload, 24)?;
                Ok(match opcode {
                    IRQMUX_OP_UNREGISTER_LINE => Self::UnregisterLine { key, line },
                    IRQMUX_OP_UNBIND_DRIVER => Self::UnbindDriver { key, line },
                    IRQMUX_OP_ENABLE => Self::Enable { key, line },
                    IRQMUX_OP_DISABLE => Self::Disable { key, line },
                    IRQMUX_OP_MASK => Self::Mask { key, line },
                    IRQMUX_OP_UNMASK => Self::Unmask { key, line },
                    IRQMUX_OP_ACK => Self::Ack { key, line },
                    _ => unreachable!(),
                })
            }
            IRQMUX_OP_INJECT_TEST_IRQ | IRQMUX_OP_GET_STATUS => {
                require_zero(&payload[..24])?;
                require_zero(&payload[28..])?;
                let line = read_u32(payload, 24)?;
                Ok(if opcode == IRQMUX_OP_INJECT_TEST_IRQ {
                    Self::InjectTestIrq { line }
                } else {
                    Self::GetStatus { line }
                })
            }
            _ => Err(IrqMuxCodecError::UnsupportedOpcode),
        }
    }
}

pub const IRQMUX_ROUTE_F_REGISTERED: u32 = 1 << 0;
pub const IRQMUX_ROUTE_F_BOUND: u32 = 1 << 1;
pub const IRQMUX_ROUTE_F_ENABLED: u32 = 1 << 2;
pub const IRQMUX_ROUTE_F_MASKED: u32 = 1 << 3;
pub const IRQMUX_ROUTE_F_AUTHORIZED: u32 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxResponse {
    pub status: IrqMuxStatus,
    pub route_flags: u32,
    pub line: IrqLine,
    pub vector: IrqVector,
    pub target: IrqRouteTarget,
    pub key: Option<IrqGrantKey>,
    pub trigger: Option<IrqTriggerMode>,
    pub polarity: Option<IrqPolarity>,
}

impl IrqMuxResponse {
    pub const ENCODED_LEN: usize = 56;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        payload[0..4].copy_from_slice(&(self.status as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&self.route_flags.to_le_bytes());
        payload[8..12].copy_from_slice(&self.line.to_le_bytes());
        payload[12..16].copy_from_slice(&self.vector.to_le_bytes());
        payload[16..24].copy_from_slice(&self.target.to_le_bytes());
        if let Some(key) = self.key {
            payload[24..32].copy_from_slice(&key.grant_id.to_le_bytes());
            payload[32..40].copy_from_slice(&key.driver_id.to_le_bytes());
            payload[40..48].copy_from_slice(&key.generation.to_le_bytes());
        }
        payload[48] = self.trigger.map_or(0, |mode| mode as u8);
        payload[49] = self.polarity.map_or(0, |polarity| polarity as u8);
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, IrqMuxCodecError> {
        if payload.len() != Self::ENCODED_LEN || payload[50..].iter().any(|byte| *byte != 0) {
            return Err(IrqMuxCodecError::Malformed);
        }
        let key = decode_key(&payload[24..48])?;
        let key = if key == IrqGrantKey::new(0, 0, 0) {
            None
        } else if key.is_valid() {
            Some(key)
        } else {
            return Err(IrqMuxCodecError::Malformed);
        };
        Ok(Self {
            status: IrqMuxStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(IrqMuxCodecError::Malformed)?,
            route_flags: read_u32(payload, 4)?,
            line: read_u32(payload, 8)?,
            vector: read_u32(payload, 12)?,
            target: read_u64(payload, 16)?,
            key,
            trigger: decode_optional_trigger(payload[48])?,
            polarity: decode_optional_polarity(payload[49])?,
        })
    }
}

fn encode_key(key: IrqGrantKey, payload: &mut [u8]) {
    payload[0..8].copy_from_slice(&key.grant_id.to_le_bytes());
    payload[8..16].copy_from_slice(&key.driver_id.to_le_bytes());
    payload[16..24].copy_from_slice(&key.generation.to_le_bytes());
}

fn encode_key_line(key: IrqGrantKey, line: IrqLine, payload: &mut [u8]) {
    encode_key(key, payload);
    payload[24..28].copy_from_slice(&line.to_le_bytes());
}

fn decode_key(payload: &[u8]) -> Result<IrqGrantKey, IrqMuxCodecError> {
    Ok(IrqGrantKey {
        grant_id: read_u64(payload, 0)?,
        driver_id: read_u64(payload, 8)?,
        generation: read_u64(payload, 16)?,
    })
}

fn decode_valid_key(payload: &[u8]) -> Result<IrqGrantKey, IrqMuxCodecError> {
    let key = decode_key(payload)?;
    if key.is_valid() {
        Ok(key)
    } else {
        Err(IrqMuxCodecError::Malformed)
    }
}

fn read_rights(payload: &[u8], offset: usize) -> Result<u32, IrqMuxCodecError> {
    let rights = read_u32(payload, offset)?;
    if rights & !IRQ_GRANT_RIGHT_ALL != 0 {
        Err(IrqMuxCodecError::UnknownRights)
    } else {
        Ok(rights)
    }
}

fn decode_optional_trigger(value: u8) -> Result<Option<IrqTriggerMode>, IrqMuxCodecError> {
    match value {
        0 => Ok(None),
        value => IrqTriggerMode::from_wire(value)
            .map(Some)
            .ok_or(IrqMuxCodecError::Malformed),
    }
}

fn decode_optional_polarity(value: u8) -> Result<Option<IrqPolarity>, IrqMuxCodecError> {
    match value {
        0 => Ok(None),
        value => IrqPolarity::from_wire(value)
            .map(Some)
            .ok_or(IrqMuxCodecError::Malformed),
    }
}

fn require_zero(payload: &[u8]) -> Result<(), IrqMuxCodecError> {
    if payload.iter().all(|byte| *byte == 0) {
        Ok(())
    } else {
        Err(IrqMuxCodecError::Malformed)
    }
}

fn read_u32(payload: &[u8], offset: usize) -> Result<u32, IrqMuxCodecError> {
    let bytes = payload
        .get(offset..offset + 4)
        .ok_or(IrqMuxCodecError::Malformed)?
        .try_into()
        .map_err(|_| IrqMuxCodecError::Malformed)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(payload: &[u8], offset: usize) -> Result<u64, IrqMuxCodecError> {
    let bytes = payload
        .get(offset..offset + 8)
        .ok_or(IrqMuxCodecError::Malformed)?
        .try_into()
        .map_err(|_| IrqMuxCodecError::Malformed)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: IrqGrantKey = IrqGrantKey::new(7, 42, 3);
    const GRANT: IrqGrantDescriptor = IrqGrantDescriptor {
        key: KEY,
        irq_line: 9,
        irq_vector: 48,
        rights: IRQ_GRANT_RIGHT_ALL,
        trigger: IrqTriggerMode::Level,
        polarity: IrqPolarity::Low,
    };

    #[test]
    fn irqmux_grant_descriptor_roundtrip() {
        assert_eq!(IrqGrantDescriptor::decode(&GRANT.encode()), Ok(GRANT));
    }

    #[test]
    fn irqmux_request_roundtrip() {
        let requests = [
            IrqMuxRequest::AuthorizeGrant { grant: GRANT },
            IrqMuxRequest::RevokeGrant { key: KEY },
            IrqMuxRequest::RegisterLine {
                key: KEY,
                line: 9,
                vector: 48,
                trigger: IrqTriggerMode::Level,
                polarity: IrqPolarity::Low,
            },
            IrqMuxRequest::UnregisterLine { key: KEY, line: 9 },
            IrqMuxRequest::BindDriver {
                key: KEY,
                line: 9,
                target: 0x1234,
            },
            IrqMuxRequest::UnbindDriver { key: KEY, line: 9 },
            IrqMuxRequest::Enable { key: KEY, line: 9 },
            IrqMuxRequest::Disable { key: KEY, line: 9 },
            IrqMuxRequest::Mask { key: KEY, line: 9 },
            IrqMuxRequest::Unmask { key: KEY, line: 9 },
            IrqMuxRequest::Ack { key: KEY, line: 9 },
            IrqMuxRequest::InjectTestIrq { line: 9 },
            IrqMuxRequest::GetStatus { line: 9 },
        ];
        for request in requests {
            let (opcode, payload) = request.encode();
            assert_eq!(IrqMuxRequest::decode(opcode, &payload), Ok(request));
        }
    }

    #[test]
    fn irqmux_response_roundtrip() {
        let response = IrqMuxResponse {
            status: IrqMuxStatus::Ok,
            route_flags: IRQMUX_ROUTE_F_REGISTERED | IRQMUX_ROUTE_F_AUTHORIZED,
            line: 9,
            vector: 48,
            target: 99,
            key: Some(KEY),
            trigger: Some(IrqTriggerMode::Level),
            polarity: Some(IrqPolarity::Low),
        };
        assert_eq!(IrqMuxResponse::decode(&response.encode()), Ok(response));
    }

    #[test]
    fn irqmux_decode_rejects_unknown_rights() {
        let mut payload = GRANT.encode();
        payload[32..36].copy_from_slice(&(IRQ_GRANT_RIGHT_ALL | (1 << 31)).to_le_bytes());
        assert_eq!(
            IrqGrantDescriptor::decode(&payload),
            Err(IrqMuxCodecError::UnknownRights)
        );
    }

    #[test]
    fn irqmux_decode_rejects_malformed_reserved_fields() {
        let (opcode, mut payload) = IrqMuxRequest::AuthorizeGrant { grant: GRANT }.encode();
        payload[63] = 1;
        assert_eq!(
            IrqMuxRequest::decode(opcode, &payload),
            Err(IrqMuxCodecError::Malformed)
        );
    }

    #[test]
    fn irqmux_abi_constants_are_stable() {
        assert_eq!(IRQMUX_ABI_VERSION, 2);
        assert_eq!(IRQMUX_OP_AUTHORIZE_GRANT, 12);
        assert_eq!(IRQMUX_OP_REVOKE_GRANT, 13);
        assert_eq!(IrqGrantDescriptor::ENCODED_LEN, 48);
        assert_eq!(IrqMuxRequest::ENCODED_LEN, 64);
        assert_eq!(IrqMuxResponse::ENCODED_LEN, 56);
    }
}
