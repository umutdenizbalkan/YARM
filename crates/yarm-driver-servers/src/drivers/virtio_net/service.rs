// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const VIRTIO_NET_TX_QUEUE_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetStats {
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub dropped_tx_packets: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetService {
    stats: VirtioNetStats,
    tx_inflight: usize,
}

impl VirtioNetService {
    pub const fn new() -> Self {
        Self {
            stats: VirtioNetStats {
                tx_packets: 0,
                rx_packets: 0,
                dropped_tx_packets: 0,
            },
            tx_inflight: 0,
        }
    }

    pub fn enqueue_tx(&mut self, packets: u64) {
        let packets = packets as usize;
        let available = VIRTIO_NET_TX_QUEUE_LIMIT.saturating_sub(self.tx_inflight);
        let accepted = available.min(packets);
        let dropped = packets.saturating_sub(accepted);

        self.tx_inflight = self.tx_inflight.saturating_add(accepted);
        self.stats.tx_packets = self.stats.tx_packets.saturating_add(accepted as u64);
        self.stats.dropped_tx_packets =
            self.stats.dropped_tx_packets.saturating_add(dropped as u64);
    }

    pub fn complete_tx(&mut self, packets: u64) {
        self.tx_inflight = self.tx_inflight.saturating_sub(packets as usize);
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
    yarm_user_rt::user_log!(
        "virtio_net.srv online: tx_packets={}, rx_packets={}, dropped_tx_packets={}",
        stats.tx_packets,
        stats.rx_packets,
        stats.dropped_tx_packets
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_net_queue_backpressure_is_deterministic() {
        let mut s = VirtioNetService::new();
        s.enqueue_tx(300);
        s.complete_rx(2);
        s.complete_tx(128);
        s.enqueue_tx(64);
        assert_eq!(
            s.stats(),
            VirtioNetStats {
                tx_packets: 320,
                rx_packets: 2,
                dropped_tx_packets: 44,
            }
        );
    }
}
