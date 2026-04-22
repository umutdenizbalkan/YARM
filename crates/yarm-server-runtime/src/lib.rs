// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[cfg(not(feature = "hosted-dev"))]
#[global_allocator]
static KERNEL_GLOBAL_ALLOCATOR: yarm::kernel::global_allocator::KernelGlobalAllocator =
    yarm::kernel::global_allocator::KERNEL_GLOBAL_ALLOCATOR;

pub mod control_plane {
    pub fn run_init_server() {
        yarm_control_plane_servers::run_init_server();
    }

    pub fn run_process_manager() {
        yarm_control_plane_servers::run_process_manager();
    }

    pub fn run_vfs_server() {
        yarm_control_plane_servers::run_vfs_server();
    }

    pub fn run_supervisor_server() {
        yarm_control_plane_servers::run_supervisor_server();
    }

    pub fn run_driver_manager_demo() {
        yarm_control_plane_servers::run_driver_manager_demo();
    }
}

pub mod fs {
    pub fn run_devfs() {
        yarm_fs_servers::run_devfs();
    }

    pub fn run_initramfs() {
        yarm_fs_servers::run_initramfs();
    }

    pub fn run_ramfs() {
        yarm_fs_servers::run_ramfs();
    }

    pub fn run_ext4() {
        yarm_fs_servers::run_ext4();
    }

    pub fn run_fat() {
        yarm_fs_servers::run_fat();
    }

    pub fn run_blkcache() {
        yarm_fs_servers::run_blkcache();
    }
}

pub mod drivers {
    pub fn run_input() {
        yarm_driver_servers::run_input();
    }

    pub fn run_irqmux() {
        yarm_driver_servers::run_irqmux();
    }

    pub fn run_uart() {
        yarm_driver_servers::run_uart();
    }

    pub fn run_virtio_blk() {
        yarm_driver_servers::run_virtio_blk();
    }

    pub fn run_virtio_gpu() {
        yarm_driver_servers::run_virtio_gpu();
    }

    pub fn run_virtio_net() {
        yarm_driver_servers::run_virtio_net();
    }
}

pub mod network {
    pub fn run_dhcp() {
        yarm_network_servers::run_dhcp();
    }

    pub fn run_dns() {
        yarm_network_servers::run_dns();
    }

    pub fn run_netmgr() {
        yarm_network_servers::run_netmgr();
    }

    pub fn run_socket() {
        yarm_network_servers::run_socket();
    }

    pub fn run_tcpip() {
        yarm_network_servers::run_tcpip();
    }
}

pub mod ui {
    pub fn run_compositor() {
        yarm_ui_servers::run_compositor();
    }

    pub fn run_display() {
        yarm_ui_servers::run_display();
    }

    pub fn run_shell() {
        yarm_ui_servers::run_shell();
    }
}

#[cfg(feature = "posix-compat")]
pub fn run_posix_compat_server() {
    yarm::compatibility::posix_compat::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_dispatch_is_workspace_crate_routed() {
        let src = include_str!("lib.rs");
        let legacy_cp = ["yarm", "::services::", "control_plane::"].concat();
        let legacy_drivers = ["yarm", "::services::", "drivers::"].concat();
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        let legacy_network = ["yarm", "::services::", "network::"].concat();
        let legacy_ui = ["yarm", "::services::", "ui::"].concat();
        assert!(
            !src.contains(legacy_cp.as_str()),
            "server-runtime control-plane dispatch must route via workspace server crate"
        );
        assert!(
            !src.contains(legacy_drivers.as_str()),
            "server-runtime driver dispatch must route via workspace server crate"
        );
        assert!(
            !src.contains(legacy_fs.as_str()),
            "server-runtime fs dispatch must route via workspace server crate"
        );
        assert!(
            !src.contains(legacy_network.as_str()),
            "server-runtime network dispatch must route via workspace server crate"
        );
        assert!(
            !src.contains(legacy_ui.as_str()),
            "server-runtime ui dispatch must route via workspace server crate"
        );
        assert!(
            src.contains("yarm_control_plane_servers::"),
            "server-runtime must depend on control-plane workspace crate dispatch"
        );
        assert!(
            src.contains("yarm_fs_servers::"),
            "server-runtime must depend on fs workspace crate dispatch"
        );
        assert!(
            src.contains("yarm_driver_servers::"),
            "server-runtime must depend on driver workspace crate dispatch"
        );
        assert!(
            src.contains("yarm_network_servers::"),
            "server-runtime must depend on network workspace crate dispatch"
        );
        assert!(
            src.contains("yarm_ui_servers::"),
            "server-runtime must depend on ui workspace crate dispatch"
        );
    }
}
