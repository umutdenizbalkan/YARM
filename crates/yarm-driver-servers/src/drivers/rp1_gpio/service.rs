// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IPC dispatch for the RP1 GPIO userspace scaffold.
//!
//! `run()` intentionally does not obtain or dereference an address from a
//! startup slot. RP1 production access is blocked on PCIe discovery and a
//! capability-controlled MMIO grant contract. The pure [`dispatch`] helper is
//! complete and hosted-mock tested so that transport can be enabled later
//! without coupling protocol correctness to hardware availability.

use crate::drivers::gpio::{GpioDeviceError, GpioDeviceOps, GpioPinMode};
use yarm_ipc_abi::gpio_abi::{
    GPIO_MODE_ALT_FUNC, GPIO_MODE_INPUT, GPIO_MODE_OUTPUT, GPIO_OP_READ_PIN, GPIO_OP_SET_FUNCTION,
    GPIO_OP_SET_PIN_MODE, GPIO_OP_WRITE_PIN, GpioReadPinReply, GpioReadPinRequest,
    GpioSetFunctionRequest, GpioSetPinModeRequest, GpioStatus, GpioStatusReply,
    GpioWritePinRequest,
};
use yarm_user_rt::ipc::Message;

/// Entry point retained for binary/build parity.
///
/// The server is not live-spawned. Enabling it requires a future RP1 PCIe BAR
/// discovery, validation, mapping, and startup-grant contract; until then this
/// function performs no MMIO and returns.
pub fn run() {
    yarm_user_rt::user_log!("RP1_GPIO_SRV_DEFERRED_NO_MMIO_GRANT");
}

fn status_for(error: GpioDeviceError) -> GpioStatus {
    match error {
        GpioDeviceError::InvalidPin => GpioStatus::InvalidPin,
        GpioDeviceError::InvalidFunction => GpioStatus::InvalidFunction,
        GpioDeviceError::Unsupported => GpioStatus::Unsupported,
    }
}

fn status_message(opcode: u16, result: Result<(), GpioDeviceError>) -> Option<Message> {
    let status = match result {
        Ok(()) => GpioStatusReply::ok(),
        Err(error) => GpioStatusReply::err(status_for(error)),
    };
    Message::with_header(0, opcode, 0, None, &status.encode()).ok()
}

/// Translate one ABI request into one reply without performing IPC syscalls.
///
/// Malformed payloads are deterministic errors, unknown operations return
/// `Unsupported`, and all device failures are translated to ABI statuses.
pub fn dispatch<D: GpioDeviceOps>(driver: &D, msg: &Message) -> Option<Message> {
    match msg.opcode {
        GPIO_OP_SET_FUNCTION => {
            let result = GpioSetFunctionRequest::decode(msg.as_slice())
                .ok_or(GpioDeviceError::InvalidPin)
                .and_then(|req| driver.set_function(req.pin as usize, req.function as u32));
            status_message(msg.opcode, result)
        }
        GPIO_OP_SET_PIN_MODE => {
            let req = match GpioSetPinModeRequest::decode(msg.as_slice()) {
                Some(req) => req,
                None => return status_message(msg.opcode, Err(GpioDeviceError::InvalidPin)),
            };
            let mode = match req.mode {
                GPIO_MODE_INPUT => GpioPinMode::Input,
                GPIO_MODE_OUTPUT => GpioPinMode::Output {
                    initial_level: req.initial_level != 0,
                },
                GPIO_MODE_ALT_FUNC => GpioPinMode::AltFunction(req.function as u32),
                _ => {
                    return Message::with_header(
                        0,
                        msg.opcode,
                        0,
                        None,
                        &GpioStatusReply::err(GpioStatus::InvalidMode).encode(),
                    )
                    .ok();
                }
            };
            status_message(msg.opcode, driver.set_pin_mode(req.pin as usize, mode))
        }
        GPIO_OP_WRITE_PIN => {
            let result = GpioWritePinRequest::decode(msg.as_slice())
                .ok_or(GpioDeviceError::InvalidPin)
                .and_then(|req| driver.write_pin(req.pin as usize, req.level != 0));
            status_message(msg.opcode, result)
        }
        GPIO_OP_READ_PIN => {
            let req = match GpioReadPinRequest::decode(msg.as_slice()) {
                Some(req) => req,
                None => return status_message(msg.opcode, Err(GpioDeviceError::InvalidPin)),
            };
            match driver.read_pin(req.pin as usize) {
                Ok(level) => Message::with_header(
                    0,
                    msg.opcode,
                    0,
                    None,
                    &GpioReadPinReply {
                        pin: req.pin,
                        level: level as u8,
                        _pad: [0; 2],
                    }
                    .encode(),
                )
                .ok(),
                Err(error) => status_message(msg.opcode, Err(error)),
            }
        }
        _ => status_message(msg.opcode, Err(GpioDeviceError::Unsupported)),
    }
}

/// Future transport loop for a platform-granted device. It is intentionally
/// not called by `run()` until the RP1 discovery/grant contract exists.
#[allow(dead_code)]
fn serve<D: GpioDeviceOps>(driver: &D, recv_cap: u32) -> ! {
    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                if let (Some(reply_cap), Some(reply)) =
                    (received.reply_cap, dispatch(driver, &received.message))
                {
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(None) => {}
            Err(_) => {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::drivers::gpio::{GpioDirection, GpioPull};
    use core::cell::{Cell, RefCell};
    use std::vec::Vec;

    #[derive(Default)]
    struct MockDriver {
        calls: RefCell<Vec<(u8, usize, u32)>>,
        read_level: Cell<bool>,
    }

    impl GpioDeviceOps for MockDriver {
        fn set_function(&self, pin: usize, function: u32) -> Result<(), GpioDeviceError> {
            if pin >= 54 {
                return Err(GpioDeviceError::InvalidPin);
            }
            if function > 31 {
                return Err(GpioDeviceError::InvalidFunction);
            }
            self.calls.borrow_mut().push((1, pin, function));
            Ok(())
        }

        fn set_pin_mode(&self, pin: usize, mode: GpioPinMode) -> Result<(), GpioDeviceError> {
            if pin >= 54 {
                return Err(GpioDeviceError::InvalidPin);
            }
            let value = match mode {
                GpioPinMode::Input => 0,
                GpioPinMode::Output { initial_level } => 1 + initial_level as u32,
                GpioPinMode::AltFunction(function) => 0x100 + function,
            };
            self.calls.borrow_mut().push((2, pin, value));
            Ok(())
        }

        fn direction(&self, _pin: usize) -> Result<GpioDirection, GpioDeviceError> {
            Ok(GpioDirection::Input)
        }

        fn write_pin(&self, pin: usize, level: bool) -> Result<(), GpioDeviceError> {
            if pin >= 54 {
                return Err(GpioDeviceError::InvalidPin);
            }
            self.calls.borrow_mut().push((3, pin, level as u32));
            Ok(())
        }

        fn read_pin(&self, pin: usize) -> Result<bool, GpioDeviceError> {
            if pin >= 54 {
                return Err(GpioDeviceError::InvalidPin);
            }
            self.calls.borrow_mut().push((4, pin, 0));
            Ok(self.read_level.get())
        }

        fn set_pull(&self, _pin: usize, _pull: GpioPull) -> Result<(), GpioDeviceError> {
            Err(GpioDeviceError::Unsupported)
        }

        fn pull(&self, _pin: usize) -> Result<GpioPull, GpioDeviceError> {
            Err(GpioDeviceError::Unsupported)
        }
    }

    fn request(opcode: u16, payload: &[u8]) -> Message {
        Message::with_header(0, opcode, 0, None, payload).unwrap()
    }

    fn status(reply: &Message) -> u8 {
        GpioStatusReply::decode(reply.as_slice()).unwrap().status
    }

    #[test]
    fn dispatch_covers_every_v1_request_and_reply() {
        let driver = MockDriver::default();
        let cases = [
            request(
                GPIO_OP_SET_FUNCTION,
                &GpioSetFunctionRequest {
                    pin: 4,
                    function: 3,
                    _pad: [0; 2],
                }
                .encode(),
            ),
            request(
                GPIO_OP_SET_PIN_MODE,
                &GpioSetPinModeRequest {
                    pin: 5,
                    mode: GPIO_MODE_OUTPUT,
                    function: 0,
                    initial_level: 1,
                }
                .encode(),
            ),
            request(
                GPIO_OP_WRITE_PIN,
                &GpioWritePinRequest {
                    pin: 6,
                    level: 1,
                    _pad: [0; 2],
                }
                .encode(),
            ),
        ];
        for msg in cases {
            assert_eq!(
                status(&dispatch(&driver, &msg).unwrap()),
                GpioStatus::Ok as u8
            );
        }

        driver.read_level.set(true);
        let read = request(
            GPIO_OP_READ_PIN,
            &GpioReadPinRequest {
                pin: 7,
                _pad: [0; 3],
            }
            .encode(),
        );
        let reply = dispatch(&driver, &read).unwrap();
        assert_eq!(GpioReadPinReply::decode(reply.as_slice()).unwrap().level, 1);
        assert_eq!(driver.calls.borrow().len(), 4);
    }

    #[test]
    fn invalid_pin_is_deterministic_for_all_pin_operations() {
        let driver = MockDriver::default();
        let messages = [
            request(GPIO_OP_SET_FUNCTION, &[54, 1, 0, 0]),
            request(GPIO_OP_SET_PIN_MODE, &[54, GPIO_MODE_INPUT, 0, 0]),
            request(GPIO_OP_WRITE_PIN, &[54, 1, 0, 0]),
            request(GPIO_OP_READ_PIN, &[54, 0, 0, 0]),
        ];
        for msg in messages {
            assert_eq!(
                status(&dispatch(&driver, &msg).unwrap()),
                GpioStatus::InvalidPin as u8
            );
        }
    }

    #[test]
    fn malformed_and_unsupported_requests_return_errors_without_panic() {
        let driver = MockDriver::default();
        let malformed = request(GPIO_OP_WRITE_PIN, &[1, 1, 0]);
        assert_eq!(
            status(&dispatch(&driver, &malformed).unwrap()),
            GpioStatus::InvalidPin as u8
        );
        let unknown = request(0xffff, &[]);
        assert_eq!(
            status(&dispatch(&driver, &unknown).unwrap()),
            GpioStatus::Unsupported as u8
        );
        let bad_mode = request(GPIO_OP_SET_PIN_MODE, &[1, 99, 0, 0]);
        assert_eq!(
            status(&dispatch(&driver, &bad_mode).unwrap()),
            GpioStatus::InvalidMode as u8
        );
    }
}
