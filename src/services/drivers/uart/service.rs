extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartStats {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartService {
    stats: UartStats,
}

impl UartService {
    pub const fn new() -> Self {
        Self {
            stats: UartStats {
                tx_bytes: 0,
                rx_bytes: 0,
            },
        }
    }

    pub fn write(&mut self, bytes: usize) {
        self.stats.tx_bytes = self.stats.tx_bytes.saturating_add(bytes as u64);
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
    println!(
        "uart.srv online: tx_bytes={}, rx_bytes={}",
        s.tx_bytes, s.rx_bytes
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_tracks_tx_and_rx_bytes() {
        let mut s = UartService::new();
        s.write(12);
        s.ingest(3);
        assert_eq!(
            s.stats(),
            UartStats {
                tx_bytes: 12,
                rx_bytes: 3
            }
        );
    }
}
