#![no_std]
extern crate std;

use std::println;

use yarm::kernel::bootstrap::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::proc_proto::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, ProcV2Args};
use yarm::kernel::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::vfs::VfsLiteService;
use yarm::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_READ, VfsV1Args};
use yarm::services::initramfs::{INITRAMFS_BUSYBOX_PATH_PTR, InitramfsBackend};

fn main() {
    let _ = Bootstrap::init().expect("kernel init");
    println!("YARM_BOOT_OK");

    let mut proc = ProcessService::new();
    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &ProcV2Args::new(1, 99).encode(),
    )
    .expect("spawn");
    let spawn_rep = proc.handle(spawn).expect("spawn rep");
    let child = SpawnV2Result::decode(spawn_rep.as_slice()).expect("child");
    proc.mark_exit(child.pid, 7).expect("mark exit");

    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &ProcV2Args::new(1, child.pid).encode(),
    )
    .expect("wait");
    let wait_rep = proc.handle(wait).expect("wait rep");
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).expect("waited");

    let mut vfs = VfsLiteService::with_backend(InitramfsBackend::new(4096));
    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, INITRAMFS_BUSYBOX_PATH_PTR, 0, 0).encode(),
    )
    .expect("open");
    let open_rep = vfs.handle_request(open).expect("open rep");
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);
    let read = Message::with_header(
        0,
        VFS_OP_READ,
        0,
        None,
        &VfsV1Args::new(fd, 0, 16, 0).encode(),
    )
    .expect("read");
    let read_rep = vfs.handle_request(read).expect("read rep");

    println!(
        "YARM_PROC_VFS_OK pid={} exit={} read_opcode={}",
        child.pid, waited.exit_code, read_rep.opcode
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_boot_markers_run() {
        main();
    }
}
