extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartStats {
    pub tx_bytes: u64,
}

pub fn run() {
    let s = UartStats { tx_bytes: 0 };
    println!("uart.srv scaffold online: tx_bytes={}", s.tx_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_stats_baseline() {
        let s = UartStats { tx_bytes: 0 };
        assert_eq!(s.tx_bytes, 0);
    }
}
