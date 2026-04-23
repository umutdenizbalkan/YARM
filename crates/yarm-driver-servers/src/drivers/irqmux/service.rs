// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

const MAX_IRQ_LINES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxStats {
    pub routed_irqs: u64,
    pub dropped_irqs: u64,
    pub masked_irqs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqRouteResult {
    Routed,
    Masked,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxService {
    stats: IrqMuxStats,
    mask: [bool; MAX_IRQ_LINES],
}

impl IrqMuxService {
    pub const fn new() -> Self {
        Self {
            stats: IrqMuxStats {
                routed_irqs: 0,
                dropped_irqs: 0,
                masked_irqs: 0,
            },
            mask: [false; MAX_IRQ_LINES],
        }
    }

    pub fn set_mask(&mut self, line: u8, masked: bool) {
        let idx = line as usize;
        if idx < MAX_IRQ_LINES {
            self.mask[idx] = masked;
        }
    }

    pub fn route_irq(&mut self, line: u8, has_consumer: bool) -> IrqRouteResult {
        let idx = line as usize;
        if idx >= MAX_IRQ_LINES {
            self.stats.dropped_irqs = self.stats.dropped_irqs.saturating_add(1);
            return IrqRouteResult::Dropped;
        }

        if self.mask[idx] {
            self.stats.masked_irqs = self.stats.masked_irqs.saturating_add(1);
            return IrqRouteResult::Masked;
        }

        if has_consumer {
            self.stats.routed_irqs = self.stats.routed_irqs.saturating_add(1);
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
    yarm_user_rt::user_log!(
        "irqmux.srv online: routed_irqs={}, dropped_irqs={}, masked_irqs={}",
        s.routed_irqs,
        s.dropped_irqs,
        s.masked_irqs
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irqmux_routes_masks_and_drops_deterministically() {
        let mut s = IrqMuxService::new();
        s.set_mask(5, true);
        assert_eq!(s.route_irq(4, true), IrqRouteResult::Routed);
        assert_eq!(s.route_irq(5, true), IrqRouteResult::Masked);
        assert_eq!(s.route_irq(7, false), IrqRouteResult::Dropped);
        assert_eq!(
            s.stats(),
            IrqMuxStats {
                routed_irqs: 1,
                dropped_irqs: 1,
                masked_irqs: 1,
            }
        );
    }
}
