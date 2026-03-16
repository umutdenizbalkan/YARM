extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetStats {
    pub tx_packets: u64,
    pub rx_packets: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetService {
    stats: VirtioNetStats,
}

impl VirtioNetService {
    pub const fn new() -> Self {
        Self {
            stats: VirtioNetStats {
                tx_packets: 0,
                rx_packets: 0,
            },
        }
    }

    pub fn enqueue_tx(&mut self, packets: u64) {
        self.stats.tx_packets = self.stats.tx_packets.saturating_add(packets);
    }

    pub fn complete_rx(&mut self, packets: u64) {
        self.stats.rx_packets = self.stats.rx_packets.saturating_add(packets);
    }

    pub const fn stats(&self) -> VirtioNetStats {
        self.stats
    }
}

pub fn run() {
    let mut s = VirtioNetService::new();
    s.enqueue_tx(1);
    let stats = s.stats();
    println!(
        "virtio_net.srv online: tx_packets={}, rx_packets={}",
        stats.tx_packets, stats.rx_packets
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_net_tracks_packet_counters() {
        let mut s = VirtioNetService::new();
        s.enqueue_tx(3);
        s.complete_rx(2);
        assert_eq!(
            s.stats(),
            VirtioNetStats {
                tx_packets: 3,
                rx_packets: 2,
            }
        );
    }
}
