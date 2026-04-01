// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const UART_TX_QUEUE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub dropped_tx_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartService {
    stats: UartStats,
    tx_inflight: usize,
}

impl UartService {
    pub const fn new() -> Self {
        Self {
            stats: UartStats {
                tx_bytes: 0,
                rx_bytes: 0,
                dropped_tx_bytes: 0,
            },
            tx_inflight: 0,
        }
    }

    pub fn write(&mut self, bytes: usize) {
        let available = UART_TX_QUEUE_LIMIT.saturating_sub(self.tx_inflight);
        let accepted = available.min(bytes);
        let dropped = bytes.saturating_sub(accepted);

        self.tx_inflight = self.tx_inflight.saturating_add(accepted);
        self.stats.tx_bytes = self.stats.tx_bytes.saturating_add(accepted as u64);
        self.stats.dropped_tx_bytes = self.stats.dropped_tx_bytes.saturating_add(dropped as u64);
    }

    pub fn complete_tx(&mut self, bytes: usize) {
        self.tx_inflight = self.tx_inflight.saturating_sub(bytes);
    }

    pub fn ingest(&mut self, bytes: usize) {
        self.stats.rx_bytes = self.stats.rx_bytes.saturating_add(bytes as u64);
    }

    pub const fn stats(&self) -> UartStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = UartService::new();
    svc.write(4);
    let s = svc.stats();
    crate::yarm_log!(
        "uart.srv online: tx_bytes={}, rx_bytes={}, dropped_tx_bytes={}",
        s.tx_bytes,
        s.rx_bytes,
        s.dropped_tx_bytes
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_backpressure_is_deterministic() {
        let mut s = UartService::new();
        s.write(80);
        s.ingest(3);
        s.complete_tx(32);
        s.write(16);
        assert_eq!(
            s.stats(),
            UartStats {
                tx_bytes: 80,
                rx_bytes: 3,
                dropped_tx_bytes: 16,
            }
        );
    }
}
