// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

use yarm::yarm_control_plane_servers::init::{
    InitRuntimeBootConfig, run_minimum_profile_with_kernel,
};

fn main() {
    let mut kernel = yarm::kernel::boot::Bootstrap::init().expect("init");
    let summary = run_minimum_profile_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
        .expect("minimum profile");

    yarm::yarm_log!(
        "core profile smoke ok: init_phase={:?}, managed={}, process_wait_exit={}, devfs_open_opcode={}, initramfs_read_opcode={}",
        summary.init_phase,
        summary.supervisor_managed_services,
        summary.process_wait_exit,
        summary.devfs_open_opcode,
        summary.initramfs_read_opcode
    );
}

#[cfg(test)]
mod tests {
    use yarm::init::InitBootPhase;
    use yarm::yarm_control_plane_servers::init::{InitRuntimeBootConfig, run_with_kernel};

    #[test]
    fn core_profile_smoke_path_is_stable() {
        let mut kernel = yarm::kernel::boot::Bootstrap::init().expect("init");
        let summary =
            run_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline()).expect("runtime boot");

        assert_eq!(summary.phase, InitBootPhase::Running);
        assert_eq!(summary.seeded_registrations, 3);
        assert_eq!(summary.online_cpus, 1);
    }
}
