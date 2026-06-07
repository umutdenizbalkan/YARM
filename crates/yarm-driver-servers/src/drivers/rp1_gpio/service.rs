// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! IPC service loop for the RP1 GPIO driver.
//!
//! The driver-manager spawns this server with the RP1 PCIe BAR virtual
//! address stored in startup-slot 13 (`STARTUP_SLOT_SERVICE_EXTRA_CAP_0`)
//! as a raw `u64`.  The service reads that slot, constructs the driver, and
//! enters an IPC receive loop that dispatches GPIO_OP_* messages.

use super::device::{GpioDriver, PinMode, Rp1GpioDriver, TOTAL_GPIOS};
use yarm_ipc_abi::gpio_abi::{
    GpioReadPinReply, GpioReadPinRequest, GpioSetFunctionRequest, GpioSetPinModeRequest,
    GpioStatus, GpioStatusReply, GpioWritePinRequest, GPIO_MODE_ALT_FUNC, GPIO_MODE_INPUT,
    GPIO_MODE_OUTPUT, GPIO_OP_READ_PIN, GPIO_OP_SET_FUNCTION, GPIO_OP_SET_PIN_MODE,
    GPIO_OP_WRITE_PIN,
};
use yarm_user_rt::ipc::Message;
use yarm_user_rt::runtime::{startup_arg_slot, startup_context, STARTUP_SLOT_SERVICE_EXTRA_CAP_0};

/// Run the GPIO service loop.
///
/// Blocks forever; the microkernel terminates this server by revoking its
/// receive capability.  Returns immediately if the BAR address slot is absent
/// or zero (misconfigured deployment).
pub fn run() {
    let ctx = startup_context();

    // Receive endpoint handed to us by the driver-manager.
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        return;
    };

    // The driver-manager stores the RP1 PCIe BAR virtual address in slot 13
    // before spawning this server.  Reading via `startup_arg_slot` bypasses
    // the `cap_from_slot` truncation so values > u32::MAX are preserved.
    let bar_vaddr = startup_arg_slot(STARTUP_SLOT_SERVICE_EXTRA_CAP_0)
        .unwrap_or(0) as usize;

    if bar_vaddr == 0 {
        // BAR not configured — deployment error; fail silently so the
        // supervisor can restart or report the fault.
        return;
    }

    // SAFETY: `bar_vaddr` was validated by the driver-manager as a
    // Device-nGnRE mapped BAR covering the RP1 peripheral space.
    let driver = unsafe { Rp1GpioDriver::new(bar_vaddr) };

    yarm_user_rt::user_log!("RP1_GPIO_SRV_READY");

    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let msg = received.message;
                let Some(reply_cap) = received.reply_cap else {
                    continue;
                };
                handle(&driver, &msg, reply_cap);
            }
            Ok(None) => {}
            Err(_) => {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Message dispatcher
// ---------------------------------------------------------------------------

fn handle(driver: &Rp1GpioDriver, msg: &Message, reply_cap: u32) {
    match msg.opcode {
        GPIO_OP_SET_FUNCTION => {
            let status = match GpioSetFunctionRequest::decode(msg.as_slice()) {
                Some(req) if (req.pin as usize) < TOTAL_GPIOS => {
                    driver.set_function(req.pin as usize, req.function as u32);
                    GpioStatusReply::ok()
                }
                Some(_) => GpioStatusReply::err(GpioStatus::InvalidPin),
                None => GpioStatusReply::err(GpioStatus::InvalidPin),
            };
            reply_status(reply_cap, GPIO_OP_SET_FUNCTION, status);
        }

        GPIO_OP_SET_PIN_MODE => {
            let status = match GpioSetPinModeRequest::decode(msg.as_slice()) {
                Some(req) if (req.pin as usize) < TOTAL_GPIOS => {
                    match req.mode {
                        GPIO_MODE_INPUT => {
                            driver.set_pin_mode(req.pin as usize, PinMode::Input);
                            GpioStatusReply::ok()
                        }
                        GPIO_MODE_OUTPUT => {
                            driver.set_pin_mode(
                                req.pin as usize,
                                PinMode::Output { initial_level: req.initial_level != 0 },
                            );
                            GpioStatusReply::ok()
                        }
                        GPIO_MODE_ALT_FUNC => {
                            driver.set_pin_mode(
                                req.pin as usize,
                                PinMode::AltFunction(req.function as u32),
                            );
                            GpioStatusReply::ok()
                        }
                        _ => GpioStatusReply::err(GpioStatus::InvalidMode),
                    }
                }
                Some(_) => GpioStatusReply::err(GpioStatus::InvalidPin),
                None => GpioStatusReply::err(GpioStatus::InvalidPin),
            };
            reply_status(reply_cap, GPIO_OP_SET_PIN_MODE, status);
        }

        GPIO_OP_WRITE_PIN => {
            let status = match GpioWritePinRequest::decode(msg.as_slice()) {
                Some(req) if (req.pin as usize) < TOTAL_GPIOS => {
                    driver.write_pin(req.pin as usize, req.level != 0);
                    GpioStatusReply::ok()
                }
                Some(_) => GpioStatusReply::err(GpioStatus::InvalidPin),
                None => GpioStatusReply::err(GpioStatus::InvalidPin),
            };
            reply_status(reply_cap, GPIO_OP_WRITE_PIN, status);
        }

        GPIO_OP_READ_PIN => {
            match GpioReadPinRequest::decode(msg.as_slice()) {
                Some(req) if (req.pin as usize) < TOTAL_GPIOS => {
                    let level = driver.read_pin(req.pin as usize);
                    let reply_payload =
                        GpioReadPinReply { pin: req.pin, level: level as u8, _pad: [0; 2] }
                            .encode();
                    if let Ok(reply) =
                        Message::with_header(0, GPIO_OP_READ_PIN, 0, None, &reply_payload)
                    {
                        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                    }
                }
                Some(_) | None => {
                    reply_status(
                        reply_cap,
                        GPIO_OP_READ_PIN,
                        GpioStatusReply::err(GpioStatus::InvalidPin),
                    );
                }
            }
        }

        _ => {
            // Unknown opcode — send a generic error status.
            reply_status(
                reply_cap,
                msg.opcode,
                GpioStatusReply::err(GpioStatus::Unimplemented),
            );
        }
    }
}

/// Send a `GpioStatusReply` as a reply to the given capability.
#[inline]
fn reply_status(reply_cap: u32, opcode: u16, status: GpioStatusReply) {
    if let Ok(reply) = Message::with_header(0, opcode, 0, None, &status.encode()) {
        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::gpio_abi::{GpioStatus, GpioStatusReply, GPIO_MODE_INPUT, GPIO_MODE_OUTPUT};

    #[test]
    fn gpio_status_reply_ok_encodes_zero() {
        assert_eq!(GpioStatusReply::ok().encode()[0], GpioStatus::Ok as u8);
    }

    #[test]
    fn set_function_request_decode_rejects_short_payload() {
        assert!(GpioSetFunctionRequest::decode(&[]).is_none());
        assert!(GpioSetFunctionRequest::decode(&[0; 3]).is_none());
    }

    #[test]
    fn set_pin_mode_decode_round_trips_all_modes() {
        for mode in [GPIO_MODE_INPUT, GPIO_MODE_OUTPUT, GPIO_MODE_ALT_FUNC] {
            let req = GpioSetPinModeRequest { pin: 10, mode, function: 2, initial_level: 1 };
            assert_eq!(GpioSetPinModeRequest::decode(&req.encode()), Some(req));
        }
    }

    #[test]
    fn write_pin_request_round_trips_high_and_low() {
        for level in [0u8, 1u8] {
            let req = GpioWritePinRequest { pin: 5, level, _pad: [0; 2] };
            assert_eq!(GpioWritePinRequest::decode(&req.encode()), Some(req));
        }
    }

    #[test]
    fn read_pin_reply_encodes_pin_and_level() {
        let reply = GpioReadPinReply { pin: 17, level: 1, _pad: [0; 2] };
        let enc = reply.encode();
        assert_eq!(enc[0], 17);
        assert_eq!(enc[1], 1);
    }
}
