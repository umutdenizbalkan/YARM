// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod drivers;
pub use drivers::{
    blkcache, firmware, input, irqmux, mailbox, rp1_gpio, uart, virtio_blk, virtio_gpu, virtio_net,
};

pub fn run_input() {
    drivers::input::run();
}

pub fn run_rp1_gpio() {
    drivers::rp1_gpio::run();
}

pub fn run_irqmux() {
    drivers::irqmux::run();
}

pub fn run_uart() {
    drivers::uart::run();
}

pub fn run_virtio_blk() {
    drivers::virtio_blk::run();
}

pub fn run_virtio_gpu() {
    drivers::virtio_gpu::run();
}

pub fn run_virtio_net() {
    drivers::virtio_net::run();
}

pub fn run_blkcache_srv() {
    drivers::blkcache::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_driver_impl_does_not_delegate_back_to_legacy_driver_namespace() {
        let input_src = include_str!("drivers/input/service.rs");
        let irqmux_src = include_str!("drivers/irqmux/service.rs");
        let uart_src = include_str!("drivers/uart/service.rs");
        let virtio_blk_src = include_str!("drivers/virtio_blk/service.rs");
        let blkcache_src = include_str!("drivers/blkcache/service.rs");
        let virtio_gpu_src = include_str!("drivers/virtio_gpu/service.rs");
        let virtio_net_src = include_str!("drivers/virtio_net/service.rs");
        let rp1_gpio_src = include_str!("drivers/rp1_gpio/service.rs");
        let legacy_drivers = ["yarm", "::services::", "drivers::"].concat();

        for src in [
            input_src,
            irqmux_src,
            uart_src,
            virtio_blk_src,
            blkcache_src,
            virtio_gpu_src,
            virtio_net_src,
            rp1_gpio_src,
        ] {
            assert!(
                !src.contains(legacy_drivers.as_str()),
                "workspace scoped drivers impl must not delegate to legacy driver namespace"
            );
        }
    }

    #[test]
    fn platform_specific_driver_layers_are_explicit_and_compatibility_is_retained() {
        let uart_service = include_str!("drivers/uart/service.rs");
        let uart_module = include_str!("drivers/uart/mod.rs");
        let pl011_device = include_str!("drivers/uart/backend/pl011/device.rs");
        let rpi_module = include_str!("drivers/firmware/rpi/mod.rs");
        let mailbox_compat = include_str!("drivers/mailbox/mod.rs");
        let audit = include_str!("../../../doc/driver-layering-audit.md");
        let manifest = include_str!("../Cargo.toml");
        let crate_root = include_str!("lib.rs");
        let concrete_uart = ["Pl011", "UartDevice"].concat();

        assert!(!uart_service.contains(&concrete_uart));
        assert!(uart_service.contains("UartDeviceOps"));
        assert!(uart_module.contains("pub mod backend"));
        assert!(pl011_device.contains("impl<B: UartRegisterIo> UartDeviceOps"));
        assert!(rpi_module.contains("Raspberry Pi / VideoCore"));
        assert!(mailbox_compat.contains("Compatibility aliases"));
        assert!(audit.contains("not live-spawned"));
        assert!(audit.contains("No `rpi_firmware_srv` bin is added"));
        assert!(!manifest.contains("name = \"rpi_firmware_srv\""));
        let firmware_run = ["run_rpi", "_firmware"].concat();
        assert!(!crate_root.contains(&firmware_run));
    }

    #[test]
    fn block_service_is_trait_backed_and_virtio_paths_remain_compatible() {
        let service = include_str!("drivers/virtio_blk/service.rs");
        let module = include_str!("drivers/virtio_blk/mod.rs");
        let backend = include_str!("drivers/virtio_blk/backend/virtio/device.rs");
        let audit = include_str!("../../../doc/driver-layering-audit.md");
        let queue_type = ["Virtq", "Chain"].concat();
        let concrete_device = ["VirtioBlk", "MemoryDevice"].concat();

        assert!(service.contains("BlockDeviceOps"));
        assert!(!service.contains(&queue_type));
        assert!(!service.contains(&concrete_device));
        assert!(backend.contains("impl<const SECTORS: usize> BlockDeviceOps"));
        assert!(module.contains("Compatibility module for the former `virtio_blk::device` path"));
        assert!(module.contains("pub type VirtioBlkWriteService"));
        assert!(audit.contains("block_backend_abi"));
        assert!(audit.contains("VIRTIO_BLK_SRV_READY"));
        assert!(audit.contains("FAT gates"));
    }

    #[test]
    fn driver_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
            (
                "blkcache_srv",
                "name = \"blkcache_srv\"",
                "path = \"src/bin/blkcache_srv.rs\"",
                "bin/blkcache_srv.rs",
                "run_blkcache_srv",
            ),
            (
                "input_srv",
                "name = \"input_srv\"",
                "path = \"src/bin/input_srv.rs\"",
                "bin/input_srv.rs",
                "run_input",
            ),
            (
                "irqmux_srv",
                "name = \"irqmux_srv\"",
                "path = \"src/bin/irqmux_srv.rs\"",
                "bin/irqmux_srv.rs",
                "run_irqmux",
            ),
            (
                "uart_srv",
                "name = \"uart_srv\"",
                "path = \"src/bin/uart_srv.rs\"",
                "bin/uart_srv.rs",
                "run_uart",
            ),
            (
                "virtio_blk_srv",
                "name = \"virtio_blk_srv\"",
                "path = \"src/bin/virtio_blk_srv.rs\"",
                "bin/virtio_blk_srv.rs",
                "run_virtio_blk",
            ),
            (
                "virtio_gpu_srv",
                "name = \"virtio_gpu_srv\"",
                "path = \"src/bin/virtio_gpu_srv.rs\"",
                "bin/virtio_gpu_srv.rs",
                "run_virtio_gpu",
            ),
            (
                "virtio_net_srv",
                "name = \"virtio_net_srv\"",
                "path = \"src/bin/virtio_net_srv.rs\"",
                "bin/virtio_net_srv.rs",
                "run_virtio_net",
            ),
            (
                "rp1_gpio_srv",
                "name = \"rp1_gpio_srv\"",
                "path = \"src/bin/rp1_gpio_srv.rs\"",
                "bin/rp1_gpio_srv.rs",
                "run_rp1_gpio",
            ),
        ];

        for (bin_name, name_entry, path_entry, bin_path, run_fn) in expected_bins {
            assert!(
                cargo_toml.contains(name_entry),
                "Cargo.toml missing expected bin entry: {bin_name}"
            );
            assert!(
                cargo_toml.contains(path_entry),
                "Cargo.toml missing expected bin path for: {bin_name}"
            );

            let src = match bin_path {
                "bin/input_srv.rs" => include_str!("bin/input_srv.rs"),
                "bin/blkcache_srv.rs" => include_str!("bin/blkcache_srv.rs"),
                "bin/irqmux_srv.rs" => include_str!("bin/irqmux_srv.rs"),
                "bin/uart_srv.rs" => include_str!("bin/uart_srv.rs"),
                "bin/virtio_blk_srv.rs" => include_str!("bin/virtio_blk_srv.rs"),
                "bin/virtio_gpu_srv.rs" => include_str!("bin/virtio_gpu_srv.rs"),
                "bin/virtio_net_srv.rs" => include_str!("bin/virtio_net_srv.rs"),
                "bin/rp1_gpio_srv.rs" => include_str!("bin/rp1_gpio_srv.rs"),
                _ => panic!("unexpected bin path in parity table: {bin_path}"),
            };
            assert!(
                src.contains("yarm_driver_servers::"),
                "{bin_name} should dispatch via yarm_driver_servers crate entrypoint"
            );
            assert!(
                src.contains(run_fn),
                "{bin_name} should call {run_fn} for parity with driver service mapping"
            );
        }
    }
}
