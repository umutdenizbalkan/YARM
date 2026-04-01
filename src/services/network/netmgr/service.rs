// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrStats {
    pub links_up: u64,
    pub links_down: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetmgrService {
    stats: NetmgrStats,
}

impl NetmgrService {
    pub const fn new() -> Self {
        Self {
            stats: NetmgrStats {
                links_up: 0,
                links_down: 0,
            },
        }
    }

    pub fn mark_link(&mut self, up: bool) {
        if up {
            self.stats.links_up = self.stats.links_up.saturating_add(1);
        } else {
            self.stats.links_down = self.stats.links_down.saturating_add(1);
        }
    }

    pub const fn stats(&self) -> NetmgrStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = NetmgrService::new();
    svc.mark_link(true);
    let s = svc.stats();
    crate::yarm_log!(
        "netmgr.srv online: links_up={}, links_down={}",
        s.links_up,
        s.links_down
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netmgr_tracks_link_state_events() {
        let mut svc = NetmgrService::new();
        svc.mark_link(true);
        svc.mark_link(false);
        assert_eq!(
            svc.stats(),
            NetmgrStats {
                links_up: 1,
                links_down: 1,
            }
        );
    }
}
