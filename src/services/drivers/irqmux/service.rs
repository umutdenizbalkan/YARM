extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxStats {
    pub routed_irqs: u64,
    pub dropped_irqs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqRouteResult {
    Routed,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxService {
    stats: IrqMuxStats,
}

impl IrqMuxService {
    pub const fn new() -> Self {
        Self {
            stats: IrqMuxStats {
                routed_irqs: 0,
                dropped_irqs: 0,
            },
        }
    }

    pub fn route_irq(&mut self, line: u8, has_consumer: bool) -> IrqRouteResult {
        if has_consumer {
            self.stats.routed_irqs = self.stats.routed_irqs.saturating_add(1);
            let _ = line;
            IrqRouteResult::Routed
        } else {
            self.stats.dropped_irqs = self.stats.dropped_irqs.saturating_add(1);
            IrqRouteResult::Dropped
        }
    }

    pub const fn stats(&self) -> IrqMuxStats {
        self.stats
    }
}

pub fn run() {
    let mut svc = IrqMuxService::new();
    let _ = svc.route_irq(1, true);
    let s = svc.stats();
    println!(
        "irqmux.srv online: routed_irqs={}, dropped_irqs={}",
        s.routed_irqs, s.dropped_irqs
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irqmux_routes_and_drops_deterministically() {
        let mut s = IrqMuxService::new();
        assert_eq!(s.route_irq(4, true), IrqRouteResult::Routed);
        assert_eq!(s.route_irq(5, false), IrqRouteResult::Dropped);
        assert_eq!(
            s.stats(),
            IrqMuxStats {
                routed_irqs: 1,
                dropped_irqs: 1
            }
        );
    }
}
