// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

extern crate alloc;

pub mod control_plane;

pub fn run_init_server() {
    #[cfg(feature = "legacy-tests")]
    control_plane::init::run();
}

pub fn run_process_manager() {
    #[cfg(feature = "legacy-tests")]
    control_plane::process_manager::run();
}

pub fn run_vfs_server() {
    control_plane::vfs::run();
}

pub fn run_supervisor_server() {
    #[cfg(feature = "legacy-tests")]
    control_plane::supervisor::run();
}

pub fn run_driver_manager() {
    #[cfg(feature = "legacy-tests")]
    control_plane::driver_manager::run();
}

pub fn run_driver_manager_demo() {
    #[cfg(feature = "legacy-tests")]
    {
    use crate::control_plane::driver_manager::DriverService;
    use yarm_ipc_abi::driver_abi::{DRIVER_OP_GRANT_IRQ, DRIVER_OP_REGISTER, pack_driver_pair};
    use yarm_user_rt::capability::CapId;
    use yarm_user_rt::ipc::Message;
    use yarm_user_rt::runtime::{DriverControlOps, KernelIpcError};

    struct DemoDriverControl;

    impl DriverControlOps for DemoDriverControl {
        fn register_driver(&mut self, _tid: u64) -> Result<(), KernelIpcError> {
            Ok(())
        }

        fn mint_irq_cap(&mut self, line: u16) -> Result<CapId, KernelIpcError> {
            Ok(CapId(0x1000 + line as u64))
        }

        fn grant_driver_irq(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
            Ok(())
        }

        fn mint_dma_region_cap(
            &mut self,
            mem_cap: CapId,
            offset: usize,
            len: usize,
        ) -> Result<CapId, KernelIpcError> {
            Ok(CapId(
                mem_cap
                    .0
                    .saturating_add(offset as u64)
                    .saturating_add(len as u64),
            ))
        }

        fn grant_driver_dma(&mut self, _tid: u64, _cap: CapId) -> Result<(), KernelIpcError> {
            Ok(())
        }

        fn restart_task(&mut self, _tid: u64, _token: u64) -> Result<(), KernelIpcError> {
            Ok(())
        }
    }

    let register = Message::with_header(0, DRIVER_OP_REGISTER, 0, None, &2u64.to_le_bytes())
        .expect("register msg");
    let grant = Message::with_header(0, DRIVER_OP_GRANT_IRQ, 0, None, &pack_driver_pair(2, 9))
        .expect("grant msg");

    let mut service = DriverService::new();
    let mut runtime = DemoDriverControl;
    let handled = service
        .handle_batch(&mut runtime, [register, grant])
        .expect("batch");

    yarm_user_rt::user_log!("driver-manager demo ready: handled={}", handled);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_plane_impl_does_not_delegate_back_to_legacy_control_plane_or_fs_namespaces() {
        let init_src = include_str!("control_plane/init/service.rs");
        let proc_src = include_str!("control_plane/process_manager/service.rs");
        let sup_src = include_str!("control_plane/supervisor/service.rs");
        let vfs_src = include_str!("control_plane/vfs/service.rs");
        let dm_src = include_str!("control_plane/driver_manager/service.rs");
        let legacy_cp = ["yarm", "::services::", "control_plane::"].concat();
        let legacy_fs = ["yarm", "::services::", "fs::"].concat();
        for src in [init_src, proc_src, sup_src, vfs_src, dm_src] {
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
