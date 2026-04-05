// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

pub mod control_plane {
    pub fn run_init_server() {
        yarm::services::control_plane::init::run();
    }

    pub fn run_process_manager() {
        yarm::services::control_plane::process_manager::run();
    }

    pub fn run_vfs_server() {
        yarm::services::control_plane::vfs::run();
    }

    pub fn run_supervisor_server() {
        yarm::services::control_plane::supervisor::run();
    }

    pub fn run_driver_manager_demo() {
        use yarm::kernel::boot::Bootstrap;
        use yarm::kernel::driver_abi::{DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, pack_driver_pair};
        use yarm::kernel::driver_manager::DriverService;
        use yarm::kernel::ipc::Message;

        let mut kernel = Bootstrap::init().expect("init");
        kernel.register_task(2).expect("task");

        let register = Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &2u64.to_le_bytes())
            .expect("register msg");
        let grant = Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(2, 9))
            .expect("grant msg");

        let mut service = DriverService::new();
        let handled = service
            .handle_batch(&mut kernel, [register, grant])
            .expect("batch");

        yarm::yarm_log!("driver-manager demo ready: handled={}", handled);
    }
}

pub mod fs {
    pub fn run_devfs() {
        yarm::services::fs::devfs::run();
    }

    pub fn run_initramfs() {
        yarm::services::fs::initramfs::run();
    }

    pub fn run_ramfs() {
        yarm::services::fs::ramfs::run();
    }

    pub fn run_ext4() {
        yarm::services::fs::ext4::run();
    }

    pub fn run_fat() {
        yarm::services::fs::fat::run();
    }

    pub fn run_blkcache() {
        yarm::services::fs::blkcache::run();
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
