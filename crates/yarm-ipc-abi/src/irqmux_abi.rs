// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Userspace IRQ multiplexer service protocol.
//!
//! This ABI configures userspace route state only. It does not describe kernel
//! interrupt delivery, interrupt-controller programming, or syscall behavior.

pub const IRQMUX_ABI_VERSION: u16 = 1;

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

pub type IrqLine = u32;
pub type IrqVector = u32;
pub type IrqRouteTarget = u64;

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
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMuxCodecError {
    Malformed,
    UnsupportedOpcode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMuxRequest {
    RegisterLine {
        line: IrqLine,
        vector: IrqVector,
        trigger: IrqTriggerMode,
        polarity: IrqPolarity,
    },
    UnregisterLine {
        line: IrqLine,
    },
    BindDriver {
        line: IrqLine,
        target: IrqRouteTarget,
    },
    UnbindDriver {
        line: IrqLine,
    },
    Enable {
        line: IrqLine,
    },
    Disable {
        line: IrqLine,
    },
    Mask {
        line: IrqLine,
    },
    Unmask {
        line: IrqLine,
    },
    Ack {
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
    pub const ENCODED_LEN: usize = 24;

    pub const fn line(self) -> IrqLine {
        match self {
            Self::RegisterLine { line, .. }
            | Self::UnregisterLine { line }
            | Self::BindDriver { line, .. }
            | Self::UnbindDriver { line }
            | Self::Enable { line }
            | Self::Disable { line }
            | Self::Mask { line }
            | Self::Unmask { line }
            | Self::Ack { line }
            | Self::InjectTestIrq { line }
            | Self::GetStatus { line } => line,
        }
    }

    pub fn encode(self) -> (u16, [u8; Self::ENCODED_LEN]) {
        let mut payload = [0u8; Self::ENCODED_LEN];
        let (opcode, line) = match self {
            Self::RegisterLine {
                line,
                vector,
                trigger,
                polarity,
            } => {
                payload[4..8].copy_from_slice(&vector.to_le_bytes());
                payload[16] = trigger as u8;
                payload[17] = polarity as u8;
                (IRQMUX_OP_REGISTER_LINE, line)
            }
            Self::UnregisterLine { line } => (IRQMUX_OP_UNREGISTER_LINE, line),
            Self::BindDriver { line, target } => {
                payload[8..16].copy_from_slice(&target.to_le_bytes());
                (IRQMUX_OP_BIND_DRIVER, line)
            }
            Self::UnbindDriver { line } => (IRQMUX_OP_UNBIND_DRIVER, line),
            Self::Enable { line } => (IRQMUX_OP_ENABLE, line),
            Self::Disable { line } => (IRQMUX_OP_DISABLE, line),
            Self::Mask { line } => (IRQMUX_OP_MASK, line),
            Self::Unmask { line } => (IRQMUX_OP_UNMASK, line),
            Self::Ack { line } => (IRQMUX_OP_ACK, line),
            Self::InjectTestIrq { line } => (IRQMUX_OP_INJECT_TEST_IRQ, line),
            Self::GetStatus { line } => (IRQMUX_OP_GET_STATUS, line),
        };
        payload[0..4].copy_from_slice(&line.to_le_bytes());
        (opcode, payload)
    }

    pub fn decode(opcode: u16, payload: &[u8]) -> Result<Self, IrqMuxCodecError> {
        if payload.len() != Self::ENCODED_LEN {
            return Err(IrqMuxCodecError::Malformed);
        }
        let line = read_u32(payload, 0)?;
        let vector = read_u32(payload, 4)?;
        let target = read_u64(payload, 8)?;
        let trigger = payload[16];
        let polarity = payload[17];
        let reserved = &payload[18..24];
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(IrqMuxCodecError::Malformed);
        }

        match opcode {
            IRQMUX_OP_REGISTER_LINE => {
                if target != 0 {
                    return Err(IrqMuxCodecError::Malformed);
                }
                Ok(Self::RegisterLine {
                    line,
                    vector,
                    trigger: IrqTriggerMode::from_wire(trigger)
                        .ok_or(IrqMuxCodecError::Malformed)?,
                    polarity: IrqPolarity::from_wire(polarity)
                        .ok_or(IrqMuxCodecError::Malformed)?,
                })
            }
            IRQMUX_OP_BIND_DRIVER => {
                require_zero_metadata(vector, trigger, polarity)?;
                if target == 0 {
                    return Err(IrqMuxCodecError::Malformed);
                }
                Ok(Self::BindDriver { line, target })
            }
            IRQMUX_OP_UNREGISTER_LINE
            | IRQMUX_OP_UNBIND_DRIVER
            | IRQMUX_OP_ENABLE
            | IRQMUX_OP_DISABLE
            | IRQMUX_OP_MASK
            | IRQMUX_OP_UNMASK
            | IRQMUX_OP_ACK
            | IRQMUX_OP_INJECT_TEST_IRQ
            | IRQMUX_OP_GET_STATUS => {
                require_zero_metadata(vector, trigger, polarity)?;
                if target != 0 {
                    return Err(IrqMuxCodecError::Malformed);
                }
                Ok(match opcode {
                    IRQMUX_OP_UNREGISTER_LINE => Self::UnregisterLine { line },
                    IRQMUX_OP_UNBIND_DRIVER => Self::UnbindDriver { line },
                    IRQMUX_OP_ENABLE => Self::Enable { line },
                    IRQMUX_OP_DISABLE => Self::Disable { line },
                    IRQMUX_OP_MASK => Self::Mask { line },
                    IRQMUX_OP_UNMASK => Self::Unmask { line },
                    IRQMUX_OP_ACK => Self::Ack { line },
                    IRQMUX_OP_INJECT_TEST_IRQ => Self::InjectTestIrq { line },
                    IRQMUX_OP_GET_STATUS => Self::GetStatus { line },
                    _ => unreachable!(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxResponse {
    pub status: IrqMuxStatus,
    pub route_flags: u32,
    pub line: IrqLine,
    pub vector: IrqVector,
    pub target: IrqRouteTarget,
    pub trigger: Option<IrqTriggerMode>,
    pub polarity: Option<IrqPolarity>,
}

impl IrqMuxResponse {
    pub const ENCODED_LEN: usize = 32;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut payload = [0u8; Self::ENCODED_LEN];
        payload[0..4].copy_from_slice(&(self.status as u32).to_le_bytes());
        payload[4..8].copy_from_slice(&self.route_flags.to_le_bytes());
        payload[8..12].copy_from_slice(&self.line.to_le_bytes());
        payload[12..16].copy_from_slice(&self.vector.to_le_bytes());
        payload[16..24].copy_from_slice(&self.target.to_le_bytes());
        payload[24] = self.trigger.map_or(0, |mode| mode as u8);
        payload[25] = self.polarity.map_or(0, |polarity| polarity as u8);
        payload
    }

    pub fn decode(payload: &[u8]) -> Result<Self, IrqMuxCodecError> {
        if payload.len() != Self::ENCODED_LEN || payload[26..32].iter().any(|byte| *byte != 0) {
            return Err(IrqMuxCodecError::Malformed);
        }
        let trigger = match payload[24] {
            0 => None,
            value => Some(IrqTriggerMode::from_wire(value).ok_or(IrqMuxCodecError::Malformed)?),
        };
        let polarity = match payload[25] {
            0 => None,
            value => Some(IrqPolarity::from_wire(value).ok_or(IrqMuxCodecError::Malformed)?),
        };
        Ok(Self {
            status: IrqMuxStatus::from_wire(read_u32(payload, 0)?)
                .ok_or(IrqMuxCodecError::Malformed)?,
            route_flags: read_u32(payload, 4)?,
            line: read_u32(payload, 8)?,
            vector: read_u32(payload, 12)?,
            target: read_u64(payload, 16)?,
            trigger,
            polarity,
        })
    }
}

fn require_zero_metadata(
    vector: IrqVector,
    trigger: u8,
    polarity: u8,
) -> Result<(), IrqMuxCodecError> {
    if vector == 0 && trigger == 0 && polarity == 0 {
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

    #[test]
    fn irqmux_request_roundtrip() {
        let requests = [
            IrqMuxRequest::RegisterLine {
                line: 9,
                vector: 48,
                trigger: IrqTriggerMode::Level,
                polarity: IrqPolarity::Low,
            },
            IrqMuxRequest::UnregisterLine { line: 9 },
            IrqMuxRequest::BindDriver {
                line: 9,
                target: 0x1234,
            },
            IrqMuxRequest::UnbindDriver { line: 9 },
            IrqMuxRequest::Enable { line: 9 },
            IrqMuxRequest::Disable { line: 9 },
            IrqMuxRequest::Mask { line: 9 },
            IrqMuxRequest::Unmask { line: 9 },
            IrqMuxRequest::Ack { line: 9 },
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
            route_flags: IRQMUX_ROUTE_F_REGISTERED | IRQMUX_ROUTE_F_BOUND,
            line: 4,
            vector: 37,
            target: 99,
            trigger: Some(IrqTriggerMode::Edge),
            polarity: Some(IrqPolarity::High),
        };
        assert_eq!(IrqMuxResponse::decode(&response.encode()), Ok(response));
    }

    #[test]
    fn irqmux_abi_constants_are_stable() {
        assert_eq!(IRQMUX_ABI_VERSION, 1);
        assert_eq!(IRQMUX_OP_REGISTER_LINE, 1);
        assert_eq!(IRQMUX_OP_UNREGISTER_LINE, 2);
        assert_eq!(IRQMUX_OP_BIND_DRIVER, 3);
        assert_eq!(IRQMUX_OP_UNBIND_DRIVER, 4);
        assert_eq!(IRQMUX_OP_ENABLE, 5);
        assert_eq!(IRQMUX_OP_DISABLE, 6);
        assert_eq!(IRQMUX_OP_MASK, 7);
        assert_eq!(IRQMUX_OP_UNMASK, 8);
        assert_eq!(IRQMUX_OP_ACK, 9);
        assert_eq!(IRQMUX_OP_INJECT_TEST_IRQ, 10);
        assert_eq!(IRQMUX_OP_GET_STATUS, 11);
        assert_eq!(IrqMuxRequest::ENCODED_LEN, 24);
        assert_eq!(IrqMuxResponse::ENCODED_LEN, 32);
    }

    #[test]
    fn irqmux_decode_rejects_malformed_payloads() {
        let request = IrqMuxRequest::Enable { line: 1 };
        let (opcode, mut payload) = request.encode();
        assert_eq!(
            IrqMuxRequest::decode(opcode, &payload[..payload.len() - 1]),
            Err(IrqMuxCodecError::Malformed)
        );
        payload[20] = 1;
        assert_eq!(
            IrqMuxRequest::decode(opcode, &payload),
            Err(IrqMuxCodecError::Malformed)
        );
        assert_eq!(
            IrqMuxRequest::decode(u16::MAX, &[0; IrqMuxRequest::ENCODED_LEN]),
            Err(IrqMuxCodecError::UnsupportedOpcode)
        );
    }
}
