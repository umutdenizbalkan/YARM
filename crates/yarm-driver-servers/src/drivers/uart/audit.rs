// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! # `uart_srv` implementation audit matrix
//!
//! | Category | Audit result | Current status / blocker |
//! |---|---|---|
//! | Existing service | A synthetic 64-byte queue/statistics model existed | Retained for deterministic accounting and connected to internal PL011 data helpers. |
//! | Stubbed behavior | `run()` only exercised fake statistics and logged "online" | It now explicitly returns deferred without MMIO or a hardware claim. |
//! | UART ABI | No UART data/control request ABI exists | `driver_abi.rs` only defines driver-manager lifecycle/grant operations; UART wire dispatch is deferred. |
//! | Dispatch | No UART request decoder/reply encoder exists | Internal mock-tested TX/RX service helpers are provided until an ABI is designed. |
//! | Register model | Missing before this work | PL011 DR, FR, IBRD, FBRD, LCRH, CR, IMSC, ICR and required fields are defined. |
//! | Backend abstraction | Missing before this work | `UartRegisterIo`, generic `Pl011UartDevice`, hosted mock, and non-hosted-only volatile backend are implemented. |
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
            "No UART data/control request ABI exists",
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
