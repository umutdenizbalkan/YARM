#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAdapterStats {
    pub opens: u64,
    pub closes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAdapterService {
    stats: SocketAdapterStats,
}

impl SocketAdapterService {
    pub const fn new() -> Self {
        Self {
            stats: SocketAdapterStats {
                opens: 0,
                closes: 0,
            },
        }
    }

    pub fn open(&mut self) {
        self.stats.opens = self.stats.opens.saturating_add(1);
    }

    pub fn close(&mut self) {
        self.stats.closes = self.stats.closes.saturating_add(1);
    }

    pub const fn stats(&self) -> SocketAdapterStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = SocketAdapterService::new();
    svc.open();
    let s = svc.stats();
    crate::yarm_log!("socket.srv online: opens={}, closes={}", s.opens, s.closes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_adapter_roundtrip_is_accounted() {
        let mut svc = SocketAdapterService::new();
        svc.open();
        svc.close();
        assert_eq!(
            svc.stats(),
            SocketAdapterStats {
                opens: 1,
                closes: 1
            }
        );
    }
}
