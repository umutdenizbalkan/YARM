#![no_std]

use yarm::services::control_plane::init::{InitRuntimeBootConfig, run_minimum_profile_with_kernel};

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
    use super::*;
    use yarm::kernel::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ};
    use yarm::services::init::InitBootPhase;

    #[test]
    fn core_profile_smoke_path_is_stable() {
        let mut kernel = yarm::kernel::boot::Bootstrap::init().expect("init");
        let summary =
            run_minimum_profile_with_kernel(&mut kernel, InitRuntimeBootConfig::baseline())
                .expect("minimum profile");

        assert_eq!(summary.init_phase, InitBootPhase::Running);
        assert_eq!(summary.supervisor_managed_services, 3);
        assert_eq!(summary.process_wait_exit, 7);
        assert_eq!(summary.devfs_open_opcode, VFS_OP_OPENAT);
        assert_eq!(summary.initramfs_read_opcode, VFS_OP_READ);
    }
}
