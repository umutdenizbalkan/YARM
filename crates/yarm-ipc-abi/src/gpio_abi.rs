// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! RP1 GPIO driver IPC protocol — ABI version 1, frozen.
//!
//! Opcodes are in the 0x0Exx block (E for embedded/hardware-GPIO).
//! All requests and replies are encoded as little-endian byte arrays
//! sized to fit within the 128-byte IPC inline payload.

// ---------------------------------------------------------------------------
// Version and opcodes
// ---------------------------------------------------------------------------

pub const GPIO_ABI_VERSION: u16 = 1;

/// Select an alternate peripheral function for a pin.
pub const GPIO_OP_SET_FUNCTION: u16 = 0x0E01;
/// Configure pin direction (input / output / alternate function).
pub const GPIO_OP_SET_PIN_MODE: u16 = 0x0E02;
/// Drive an output pin high or low.
pub const GPIO_OP_WRITE_PIN: u16 = 0x0E03;
/// Sample the current digital level of any pin.
pub const GPIO_OP_READ_PIN: u16 = 0x0E04;

// ---------------------------------------------------------------------------
// Pin-mode discriminants (carried inline in GpioSetPinModeRequest.mode)
// ---------------------------------------------------------------------------

/// Configure pin as a high-impedance digital input.
pub const GPIO_MODE_INPUT: u8 = 0;
/// Configure pin as a push-pull digital output.
pub const GPIO_MODE_OUTPUT: u8 = 1;
/// Select a peripheral alternate function (uses GpioSetPinModeRequest.function).
pub const GPIO_MODE_ALT_FUNC: u8 = 2;

// ---------------------------------------------------------------------------
// Request / reply types
// ---------------------------------------------------------------------------

/// GPIO_OP_SET_FUNCTION — select a raw FUNCSEL value for `pin`.
///
/// `function` must be 0–31; 5 = GPIO (SYS_RIO), 0x1f = tri-state.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioSetFunctionRequest {
    pub pin: u8,
    pub function: u8,
    pub _pad: [u8; 2],
}

impl GpioSetFunctionRequest {
    pub const ENCODED_LEN: usize = 4;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.pin, self.function, 0, 0]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            pin: b[0],
            function: b[1],
            _pad: [0; 2],
        })
    }
}

/// GPIO_OP_SET_PIN_MODE — configure direction and pad parameters for `pin`.
///
/// `mode`          — one of `GPIO_MODE_*`
/// `function`      — used only when `mode == GPIO_MODE_ALT_FUNC`
/// `initial_level` — used only when `mode == GPIO_MODE_OUTPUT`; 0 = low, 1 = high
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioSetPinModeRequest {
    pub pin: u8,
    pub mode: u8,
    pub function: u8,
    pub initial_level: u8,
}

impl GpioSetPinModeRequest {
    pub const ENCODED_LEN: usize = 4;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.pin, self.mode, self.function, self.initial_level]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            pin: b[0],
            mode: b[1],
            function: b[2],
            initial_level: b[3],
        })
    }
}

/// GPIO_OP_WRITE_PIN — drive `pin` to `level` (0 = low, 1 = high).
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioWritePinRequest {
    pub pin: u8,
    pub level: u8,
    pub _pad: [u8; 2],
}

impl GpioWritePinRequest {
    pub const ENCODED_LEN: usize = 4;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.pin, self.level, 0, 0]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            pin: b[0],
            level: b[1],
            _pad: [0; 2],
        })
    }
}

/// GPIO_OP_READ_PIN — read the current digital state of `pin`.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioReadPinRequest {
    pub pin: u8,
    pub _pad: [u8; 3],
}

impl GpioReadPinRequest {
    pub const ENCODED_LEN: usize = 4;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.pin, 0, 0, 0]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            pin: b[0],
            _pad: [0; 3],
        })
    }
}

/// Reply for GPIO_OP_READ_PIN.
///
/// `level` — 0 = low, 1 = high; reflects the SYS_RIO synchronised IN register.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioReadPinReply {
    pub pin: u8,
    pub level: u8,
    pub _pad: [u8; 2],
}

impl GpioReadPinReply {
    pub const ENCODED_LEN: usize = 4;

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.pin, self.level, 0, 0]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            pin: b[0],
            level: b[1],
            _pad: [0; 2],
        })
    }
}

// ---------------------------------------------------------------------------
// Status type (used for acknowledgement replies)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum GpioStatus {
    Ok = 0,
    InvalidPin = 1,
    InvalidMode = 2,
    InvalidFunction = 3,
    Unimplemented = 255,
}

/// Generic status reply for operations that return no data (set_function,
/// set_pin_mode, write_pin).
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GpioStatusReply {
    pub status: u8,
    pub _pad: [u8; 3],
}

impl GpioStatusReply {
    pub const ENCODED_LEN: usize = 4;

    pub const fn ok() -> Self {
        Self {
            status: GpioStatus::Ok as u8,
            _pad: [0; 3],
        }
    }

    pub const fn err(s: GpioStatus) -> Self {
        Self {
            status: s as u8,
            _pad: [0; 3],
        }
    }

    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        [self.status, 0, 0, 0]
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        Some(Self {
            status: b[0],
            _pad: [0; 3],
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_abi_version_is_frozen() {
        assert_eq!(GPIO_ABI_VERSION, 1);
        assert_eq!(GPIO_OP_SET_FUNCTION, 0x0E01);
        assert_eq!(GPIO_OP_SET_PIN_MODE, 0x0E02);
        assert_eq!(GPIO_OP_WRITE_PIN, 0x0E03);
        assert_eq!(GPIO_OP_READ_PIN, 0x0E04);
    }

    #[test]
    fn set_function_request_round_trips() {
        let req = GpioSetFunctionRequest {
            pin: 17,
            function: 5,
            _pad: [0; 2],
        };
        assert_eq!(GpioSetFunctionRequest::decode(&req.encode()), Some(req));
    }

    #[test]
    fn set_pin_mode_request_round_trips() {
        for (mode, func, level) in [
            (GPIO_MODE_INPUT, 0, 0),
            (GPIO_MODE_OUTPUT, 0, 1),
            (GPIO_MODE_ALT_FUNC, 3, 0),
        ] {
            let req = GpioSetPinModeRequest {
                pin: 4,
                mode,
                function: func,
                initial_level: level,
            };
            assert_eq!(GpioSetPinModeRequest::decode(&req.encode()), Some(req));
        }
    }

    #[test]
    fn write_pin_request_round_trips() {
        for level in [0u8, 1u8] {
            let req = GpioWritePinRequest {
                pin: 27,
                level,
                _pad: [0; 2],
            };
            assert_eq!(GpioWritePinRequest::decode(&req.encode()), Some(req));
        }
    }

    #[test]
    fn read_pin_reply_round_trips() {
        let reply = GpioReadPinReply {
            pin: 3,
            level: 1,
            _pad: [0; 2],
        };
        assert_eq!(GpioReadPinReply::decode(&reply.encode()), Some(reply));
    }

    #[test]
    fn status_reply_ok_and_err_encode_correctly() {
        assert_eq!(GpioStatusReply::ok().encode()[0], 0);
        assert_eq!(GpioStatusReply::err(GpioStatus::InvalidPin).encode()[0], 1);
        assert_eq!(GpioStatusReply::err(GpioStatus::InvalidMode).encode()[0], 2);
    }

    #[test]
    fn decode_rejects_undersized_buffers() {
        assert_eq!(GpioSetFunctionRequest::decode(&[]), None);
        assert_eq!(GpioSetFunctionRequest::decode(&[0; 3]), None);
        assert_eq!(GpioReadPinRequest::decode(&[42; 1]), None);
        assert_eq!(GpioStatusReply::decode(&[0; 2]), None);
    }
}
