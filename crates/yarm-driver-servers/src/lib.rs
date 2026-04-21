// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod drivers;
pub use drivers::{input, irqmux, uart, virtio_blk, virtio_gpu, virtio_net};

pub fn run_input() {
    drivers::input::run();
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

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_driver_impl_does_not_delegate_back_to_legacy_driver_namespace() {
        let input_src = include_str!("drivers/input/service.rs");
        let irqmux_src = include_str!("drivers/irqmux/service.rs");
        let uart_src = include_str!("drivers/uart/service.rs");
        let virtio_blk_src = include_str!("drivers/virtio_blk/service.rs");
        let virtio_gpu_src = include_str!("drivers/virtio_gpu/service.rs");
        let virtio_net_src = include_str!("drivers/virtio_net/service.rs");
        let legacy_drivers = ["yarm", "::services::", "drivers::"].concat();

        for src in [
            input_src,
            irqmux_src,
            uart_src,
            virtio_blk_src,
            virtio_gpu_src,
            virtio_net_src,
        ] {
            assert!(
                !src.contains(legacy_drivers.as_str()),
                "workspace scoped drivers impl must not delegate to legacy driver namespace"
            );
        }
    }

    #[test]
    fn driver_server_bin_parity_guard_covers_expected_entrypoints() {
        let cargo_toml = include_str!("../Cargo.toml");
        let expected_bins = [
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
                "bin/irqmux_srv.rs" => include_str!("bin/irqmux_srv.rs"),
                "bin/uart_srv.rs" => include_str!("bin/uart_srv.rs"),
                "bin/virtio_blk_srv.rs" => include_str!("bin/virtio_blk_srv.rs"),
                "bin/virtio_gpu_srv.rs" => include_str!("bin/virtio_gpu_srv.rs"),
                "bin/virtio_net_srv.rs" => include_str!("bin/virtio_net_srv.rs"),
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
