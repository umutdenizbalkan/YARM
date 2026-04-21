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
        yarm::services::drivers::input::run();
    }

    pub fn run_irqmux() {
        yarm::services::drivers::irqmux::run();
    }

    pub fn run_uart() {
        yarm::services::drivers::uart::run();
    }

    pub fn run_virtio_blk() {
        yarm::services::drivers::virtio_blk::run();
    }

    pub fn run_virtio_gpu() {
        yarm::services::drivers::virtio_gpu::run();
    }

    pub fn run_virtio_net() {
        yarm::services::drivers::virtio_net::run();
    }
}

pub mod network {
    pub fn run_dhcp() {
        yarm::services::network::dhcp::run();
    }

    pub fn run_dns() {
        yarm::services::network::dns::run();
    }

    pub fn run_netmgr() {
        yarm::services::network::netmgr::run();
    }

    pub fn run_socket() {
        yarm::services::network::socket::run();
    }

    pub fn run_tcpip() {
        yarm::services::network::tcpip::run();
    }
}

pub mod ui {
    pub fn run_compositor() {
        yarm::services::ui::compositor::run();
    }

    pub fn run_display() {
        yarm::services::ui::display::run();
    }

    pub fn run_shell() {
        yarm::services::ui::shell::run();
    }
}

#[cfg(feature = "posix-compat")]
pub fn run_posix_compat_server() {
    yarm::services::compatibility::posix_compat::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_dispatch_is_workspace_crate_routed() {
        let src = include_str!("lib.rs");
        let legacy_cp = ["yarm", "::services::", "control_plane::"].concat();
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        assert!(
            !src.contains(legacy_cp.as_str()),
            "server-runtime control-plane dispatch must route via workspace server crate"
        );
        assert!(
            !src.contains(legacy_fs.as_str()),
            "server-runtime fs dispatch must route via workspace server crate"
        );
        assert!(
            src.contains("yarm_control_plane_servers::"),
            "server-runtime must depend on control-plane workspace crate dispatch"
        );
        assert!(
            src.contains("yarm_fs_servers::"),
            "server-runtime must depend on fs workspace crate dispatch"
        );
    }
}
