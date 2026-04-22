// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;

pub use yarm_fs_servers;

pub mod control_plane;

pub fn run_init_server() {
    control_plane::init::run();
}

pub fn run_process_manager() {
    control_plane::process_manager::run();
}

pub fn run_vfs_server() {
    control_plane::vfs::run();
}

pub fn run_supervisor_server() {
    control_plane::supervisor::run();
}

pub fn run_driver_manager_demo() {
    use crate::control_plane::driver_manager::DriverService;
    use yarm::kernel::boot::Bootstrap;
    use yarm::kernel::driver_abi::{DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, pack_driver_pair};
    use yarm::kernel::ipc::Message;
    use alloc::boxed::Box;

    let mut kernel = Box::new(Bootstrap::init().expect("init"));
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

#[cfg(test)]
mod tests {
    #[test]
    fn control_plane_impl_does_not_delegate_back_to_legacy_control_plane_or_fs_namespaces() {
        let init_src = include_str!("control_plane/init/service.rs");
        let proc_src = include_str!("control_plane/process_manager/service.rs");
        let sup_src = include_str!("control_plane/supervisor/service.rs");
        let vfs_src = include_str!("control_plane/vfs/service.rs");
        let legacy_cp = ["yarm", "::services::", "control_plane::"].concat();
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        for src in [init_src, proc_src, sup_src, vfs_src] {
            assert!(
                !src.contains(legacy_cp.as_str()),
                "workspace control-plane impl must not delegate to legacy control_plane namespace"
            );
            assert!(
                !src.contains(legacy_fs.as_str()),
                "workspace control-plane impl must not delegate to legacy fs namespace"
            );
        }
    }
}
