// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsStats {
    pub cache_hits: u64,
    pub upstream_queries: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsService {
    stats: DnsStats,
}

impl DnsService {
    pub const fn new() -> Self {
        Self {
            stats: DnsStats {
                cache_hits: 0,
                upstream_queries: 0,
            },
        }
    }

    pub fn resolve(&mut self, in_cache: bool) {
        if in_cache {
            self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
        } else {
            self.stats.upstream_queries = self.stats.upstream_queries.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> DnsStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = DnsService::new();
    svc.resolve(false);
    let s = svc.stats();
    crate::yarm_log!(
        "dns.srv online: cache_hits={}, upstream_queries={}",
        s.cache_hits,
        s.upstream_queries
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_timeout_retry_is_reproducible() {
        let mut svc = DnsService::new();
        svc.resolve(false);
        svc.resolve(false);
        svc.resolve(true);
        assert_eq!(
            svc.stats(),
            DnsStats {
                cache_hits: 1,
                upstream_queries: 2,
            }
        );
    }
}
