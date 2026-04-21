// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod input;
pub mod irqmux;
pub mod uart;
pub mod virtio_blk;
pub mod virtio_gpu;
pub mod virtio_net;

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_scoped_driver_modules_are_include_only_shims() {
        let input_service = include_str!("input/service.rs");
        let irqmux_service = include_str!("irqmux/service.rs");
        let uart_service = include_str!("uart/service.rs");
        let virtio_blk_service = include_str!("virtio_blk/service.rs");
        let virtio_blk_device = include_str!("virtio_blk/device.rs");
        let virtio_gpu_service = include_str!("virtio_gpu/service.rs");
        let virtio_net_service = include_str!("virtio_net/service.rs");

        assert!(input_service.contains("/crates/yarm-driver-servers/src/drivers/input/service.rs"));
        assert!(
            irqmux_service.contains("/crates/yarm-driver-servers/src/drivers/irqmux/service.rs")
        );
        assert!(uart_service.contains("/crates/yarm-driver-servers/src/drivers/uart/service.rs"));
        assert!(virtio_blk_service
            .contains("/crates/yarm-driver-servers/src/drivers/virtio_blk/service.rs"));
        assert!(virtio_blk_device
            .contains("/crates/yarm-driver-servers/src/drivers/virtio_blk/device.rs"));
        assert!(virtio_gpu_service
            .contains("/crates/yarm-driver-servers/src/drivers/virtio_gpu/service.rs"));
        assert!(virtio_net_service
            .contains("/crates/yarm-driver-servers/src/drivers/virtio_net/service.rs"));
    }
}
