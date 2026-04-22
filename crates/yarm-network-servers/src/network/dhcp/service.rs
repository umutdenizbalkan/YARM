// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpStats {
    pub leases_granted: u64,
    pub lease_renewals: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpService {
    stats: DhcpStats,
}

impl DhcpService {
    pub const fn new() -> Self {
        Self {
            stats: DhcpStats {
                leases_granted: 0,
                lease_renewals: 0,
            },
        }
    }

    pub fn grant_lease(&mut self, renew: bool) {
        if renew {
            self.stats.lease_renewals = self.stats.lease_renewals.saturating_add(1);
        } else {
            self.stats.leases_granted = self.stats.leases_granted.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> DhcpStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = DhcpService::new();
    svc.grant_lease(false);
    let s = svc.stats();
    yarm::yarm_log!(
        "dhcp.srv online: leases_granted={}, lease_renewals={}",
        s.leases_granted,
        s.lease_renewals
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dhcp_lease_accounting_is_deterministic() {
        let mut svc = DhcpService::new();
        svc.grant_lease(false);
        svc.grant_lease(true);
        assert_eq!(
            svc.stats(),
            DhcpStats {
                leases_granted: 1,
                lease_renewals: 1,
            }
        );
    }
}
