extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetStats {
    pub tx_packets: u64,
    pub rx_packets: u64,
}

pub fn run() {
    let s = VirtioNetStats {
        tx_packets: 0,
        rx_packets: 0,
    };
    println!(
        "virtio_net.srv scaffold online: tx_packets={}, rx_packets={}",
        s.tx_packets, s.rx_packets
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_net_stats_baseline() {
        let s = VirtioNetStats {
            tx_packets: 0,
            rx_packets: 0,
        };
        assert_eq!(s.tx_packets + s.rx_packets, 0);
    }
}
