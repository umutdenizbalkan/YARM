// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Driver-manager IPC protocol constants.

use crate::irqmux_abi::{
    IrqDriverId, IrqGrantDescriptor, IrqGrantGeneration, IrqGrantId, IrqLine, IrqPolarity,
    IrqTriggerMode, IrqVector,
};

pub const DRIVER_SERVER_ABI_VERSION: u16 = 1;

pub const DRIVER_OP_REGISTER: u16 = 1;
pub const DRIVER_OP_GRANT_IRQ: u16 = 2;
pub const DRIVER_OP_GRANT_DMA: u16 = 3;
pub const DRIVER_OP_RESTARTED: u16 = 4;

pub fn pack_driver_pair(a: u64, b: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..16].copy_from_slice(&b.to_le_bytes());
    out
}

/// Build the userspace IRQMUX authorization descriptor corresponding to a
/// driver-manager IRQ grant. This helper does not mint or validate a kernel
/// capability and is not used by the live driver-manager request path yet.
pub const fn make_irqmux_grant_descriptor(
    grant_id: IrqGrantId,
    driver_id: IrqDriverId,
    generation: IrqGrantGeneration,
    irq_line: IrqLine,
    irq_vector: IrqVector,
    rights: u32,
    trigger: IrqTriggerMode,
    polarity: IrqPolarity,
) -> Option<IrqGrantDescriptor> {
    let descriptor = IrqGrantDescriptor {
        key: crate::irqmux_abi::IrqGrantKey::new(grant_id, driver_id, generation),
        irq_line,
        irq_vector,
        rights,
        trigger,
        polarity,
    };
    if descriptor.is_valid() {
        Some(descriptor)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_manager_irq_grant_descriptor_matches_irqmux_contract() {
        let descriptor = make_irqmux_grant_descriptor(
            100,
            7,
            3,
            9,
            48,
            crate::irqmux_abi::IRQ_GRANT_RIGHT_ALL,
            IrqTriggerMode::Level,
            IrqPolarity::Low,
        )
        .expect("valid descriptor");
        assert_eq!(
            IrqGrantDescriptor::decode(&descriptor.encode()),
            Ok(descriptor)
        );
        assert_eq!(descriptor.key.driver_id, 7);
        assert_eq!(descriptor.irq_line, 9);
        assert_eq!(descriptor.irq_vector, 48);
    }

    #[test]
    fn driver_manager_irq_grant_descriptor_rejects_invalid_placeholder_identity() {
        assert_eq!(
            make_irqmux_grant_descriptor(
                0,
                7,
                3,
                9,
                48,
                crate::irqmux_abi::IRQ_GRANT_RIGHT_ALL,
                IrqTriggerMode::Edge,
                IrqPolarity::High,
            ),
            None
        );
    }

    #[test]
    fn driver_abi_is_frozen() {
        assert_eq!(DRIVER_SERVER_ABI_VERSION, 1);
        assert_eq!(DRIVER_OP_REGISTER, 1);
        assert_eq!(DRIVER_OP_GRANT_IRQ, 2);
        assert_eq!(DRIVER_OP_GRANT_DMA, 3);
        assert_eq!(DRIVER_OP_RESTARTED, 4);
    }
}
