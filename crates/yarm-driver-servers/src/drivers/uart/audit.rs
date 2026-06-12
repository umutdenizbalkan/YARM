// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! # `uart_srv` implementation audit matrix
//!
//! | Category | Audit result | Current status / blocker |
//! |---|---|---|
//! | Generic service | UART ABI dispatch and accounting | Depends only on `UartDeviceOps`; no PL011 type is imported. |
//! | Stubbed behavior | `run()` only exercised fake statistics and logged "online" | It now explicitly returns deferred without MMIO or a hardware claim. |
//! | UART ABI | Version 1 exists | Generic fixed-size request/reply codecs remain independent of PL011. |
//! | Dispatch | Pure generic dispatch exists | Mock-tested through `UartDeviceOps`; no live IPC loop is present. |
//! | Register model | Missing before this work | PL011 DR, FR, IBRD, FBRD, LCRH, CR, IMSC, ICR and required fields are defined. |
//! | Backend layout | PL011 was mixed into `uart/` | PL011 device/register/mock/volatile code lives under `uart/backend/pl011`. |
//! | Hosted tests | One queue-backpressure test existed | Register flags, exact writes, configuration order, errors, service helpers, and no-real-MMIO are covered. |
//! | Hardware blockers | No discovery or MMIO grant path exists | Raspberry Pi 5 needs platform DTB discovery, clock/divisor policy, pinmux ownership, and capability-granted MMIO; QEMU also needs an explicit platform grant. |
//! | Kernel early console | Existing architecture-specific kernel facility | It remains separate and untouched; `uart_srv` must never become a dependency of early boot/fatal logging. |
//!
//! `uart_srv` is not live-spawned. This implementation is mock/protocol-ready,
//! not hardware-proven. It embeds no production Raspberry Pi 5 UART address and
//! does not consume an unverified startup-slot MMIO value.

#[cfg(test)]
mod tests {
    #[test]
    fn audit_keeps_boundaries_and_blockers_explicit() {
        let audit = include_str!("audit.rs");
        for required in [
            "Version 1 exists",
            "uart/backend/pl011",
            "DTB discovery",
            "capability-granted MMIO",
            "Kernel early console",
            "not live-spawned",
            "mock/protocol-ready",
            "not hardware-proven",
        ] {
            assert!(audit.contains(required), "UART audit lost: {required}");
        }

        let service = include_str!("service.rs");
        assert!(!service.contains("startup_arg_slot"));
        assert!(!service.contains("from_granted_mapping"));
    }
}
