extern crate std;

use std::println;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqMuxStats {
    pub routed_irqs: u64,
}

pub fn run() {
    let s = IrqMuxStats { routed_irqs: 0 };
    println!("irqmux.srv scaffold online: routed_irqs={}", s.routed_irqs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irqmux_stats_baseline() {
        let s = IrqMuxStats { routed_irqs: 0 };
        assert_eq!(s.routed_irqs, 0);
    }
}
