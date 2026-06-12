// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! ARM PL011 UART register offsets and fields.
//!
//! All offsets are relative to a platform-discovered, capability-granted UART
//! MMIO mapping. This module intentionally defines no physical base address.

/// Data register.
pub const DR: usize = 0x000;
/// Flag register.
pub const FR: usize = 0x018;
/// Integer baud-rate divisor.
pub const IBRD: usize = 0x024;
/// Fractional baud-rate divisor.
pub const FBRD: usize = 0x028;
/// Line control register.
pub const LCRH: usize = 0x02c;
/// Control register.
pub const CR: usize = 0x030;
/// Interrupt mask set/clear register.
pub const IMSC: usize = 0x038;
/// Interrupt clear register.
pub const ICR: usize = 0x044;

pub mod fr {
    /// UART is transmitting data.
    pub const BUSY: u32 = 1 << 3;
    /// Receive FIFO is empty.
    pub const RXFE: u32 = 1 << 4;
    /// Transmit FIFO is full.
    pub const TXFF: u32 = 1 << 5;
}

pub mod lcrh {
    /// Enable FIFOs.
    pub const FEN: u32 = 1 << 4;
    pub const WLEN_5: u32 = 0 << 5;
    pub const WLEN_6: u32 = 1 << 5;
    pub const WLEN_7: u32 = 2 << 5;
    pub const WLEN_8: u32 = 3 << 5;
}

pub mod cr {
    /// Enable the UART.
    pub const UARTEN: u32 = 1 << 0;
    /// Enable transmission.
    pub const TXE: u32 = 1 << 8;
    /// Enable reception.
    pub const RXE: u32 = 1 << 9;
}

/// PL011 defines write-one-to-clear bits 0 through 10 in ICR.
pub const ICR_ALL: u32 = 0x07ff;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pl011_register_offsets_and_required_bits_are_stable() {
        assert_eq!(DR, 0x000);
        assert_eq!(FR, 0x018);
        assert_eq!(IBRD, 0x024);
        assert_eq!(FBRD, 0x028);
        assert_eq!(LCRH, 0x02c);
        assert_eq!(CR, 0x030);
        assert_eq!(IMSC, 0x038);
        assert_eq!(ICR, 0x044);
        assert_eq!(fr::RXFE, 1 << 4);
        assert_eq!(fr::TXFF, 1 << 5);
        assert_eq!(cr::UARTEN | cr::TXE | cr::RXE, 0x301);
    }
}
