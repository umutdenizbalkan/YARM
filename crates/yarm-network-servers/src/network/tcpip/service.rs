// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpIpStats {
    pub routed_packets: u64,
    pub dropped_packets: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpIpService {
    stats: TcpIpStats,
}

impl TcpIpService {
    pub const fn new() -> Self {
        Self {
            stats: TcpIpStats {
                routed_packets: 0,
                dropped_packets: 0,
            },
        }
    }

    pub fn route_packet(&mut self, route_exists: bool) {
        if route_exists {
            self.stats.routed_packets = self.stats.routed_packets.saturating_add(1);
        } else {
            self.stats.dropped_packets = self.stats.dropped_packets.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> TcpIpStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = TcpIpService::new();
    svc.route_packet(true);
    let s = svc.stats();
    yarm_user_rt::user_log!(
        "tcpip.srv online: routed_packets={}, dropped_packets={}",
        s.routed_packets,
        s.dropped_packets
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcpip_deterministic_packet_path() {
        let mut svc = TcpIpService::new();
        svc.route_packet(true);
        svc.route_packet(false);
        assert_eq!(
            svc.stats(),
            TcpIpStats {
                routed_packets: 1,
                dropped_packets: 1,
            }
        );
    }
}
