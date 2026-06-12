// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! # RP1 GPIO implementation audit matrix
//!
//! | Category | State after audit | Verification / blocker |
//! |---|---|---|
//! | ABI requests/replies | Implemented for ABI v1 set-function, set-mode, write, and read | Codec and dispatch tests cover every opcode. ABI has no pull, direction-query, or interrupt opcode. |
//! | Generic GPIO contract | Implemented | `drivers/gpio` defines `GpioDeviceOps` plus shared error, direction, mode, and pull types. |
//! | RP1 implementation | Implemented | `Rp1GpioDevice<B>` implements `GpioDeviceOps` and remains under `drivers/rp1_gpio`. |
//! | Service dispatch | Implemented as a pure, mock-testable request-to-reply function | Runtime transport remains separate. Unknown opcodes return `Unsupported`. |
//! | Register constants/bitfields | Implemented for GPIO_CTRL, SYS_RIO, and PADS | Layout assertions and exact-offset mock tests exist. Offsets are BAR-relative, not production physical addresses. |
//! | MMIO abstraction/mock | Implemented | `RegisterIo` plus hosted mock; volatile backend is excluded from hosted-dev. |
//! | Pin validation | Implemented | Pins 0..=53 accepted; all others deterministically return `InvalidPin`. |
//! | Direction input/output | Implemented at device level and set-mode ABI | Direction query is device-only because ABI v1 has no query opcode. |
//! | Level read/write | Implemented | Exact SYS_RIO bank, alias, and bit behavior is mock-tested. |
//! | Pull off/up/down | Implemented at device level | ABI v1 has no pull opcode, so service exposure is deferred rather than extending the frozen ABI here. |
//! | Alternate functions | Implemented for documented alternate selectors 1..=4 and 6..=8 | Invalid selectors return `InvalidFunction`. |
//! | Interrupt config/status/ack | Missing/deferred | ABI v1 and the current verified register model do not define these operations. |
//! | Error handling | Implemented | Invalid pin/mode/function and unsupported opcode are explicit; dispatch does not panic. |
//! | Hosted-dev tests | Implemented | Mock register and request/reply translation tests require no hardware. |
//! | No real MMIO in hosted-dev | Implemented | The volatile backend is compile-time excluded and `run()` does not consume an address slot. |
//!
//! RP1 is the Raspberry Pi 5 / CM5 I/O controller attached to BCM2712 over
//! PCIe, not a standalone target reached through assumed BCM2712 MMIO. The
//! `rp1_gpio_srv` binary is scaffolded but is not live-spawned. Production use
//! remains blocked on RP1 PCIe discovery, BAR sizing/validation, capability-
//! controlled MMIO mapping/grant, interrupt routing, and an explicit startup
//! contract carrying that grant. Consequently this code is mock/protocol-ready,
//! not hardware-proven.

#[cfg(test)]
mod tests {
    #[test]
    fn audit_matrix_keeps_production_blockers_explicit() {
        let audit = include_str!("audit.rs");
        for required in [
            "PCIe discovery",
            "MMIO mapping/grant",
            "not live-spawned",
            "mock/protocol-ready",
            "not hardware-proven",
            "No real MMIO in hosted-dev",
            "GpioDeviceOps",
            "drivers/rp1_gpio",
        ] {
            assert!(audit.contains(required), "audit matrix lost: {required}");
        }
    }
}
