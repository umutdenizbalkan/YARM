#![no_std]
use yarm::kernel::ipc::Message;
use yarm::kernel::proc_proto::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, ProcV2Args};
use yarm::kernel::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::vfs::VfsLiteService;
use yarm::kernel::vfs_proto::{VFS_OP_OPENAT, VFS_OP_READ, VfsV1Args};


fn main() {
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

    let mut vfs = VfsLiteService::new();
    let open = Message::with_header(
        0,
        VFS_OP_OPENAT,
        0,
        None,
        &VfsV1Args::new(0, 0x1000, 0, 0).encode(),
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
        &VfsV1Args::new(fd, 0x2000, 16, 0).encode(),
    )
    .expect("read");
    let _ = vfs.handle_request(read).expect("read rep");

    yarm::yarm_log!(
        "core profile smoke ok: child_pid={}, waited_exit={}, proc_handled={}",
        child.pid,
        waited.exit_code,
        proc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_profile_smoke_path_is_stable() {
        let mut proc = ProcessService::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &ProcV2Args::new(1, 9).encode(),
        )
        .expect("spawn");
        let spawn_rep = proc.handle(spawn).expect("spawn rep");
        let child = SpawnV2Result::decode(spawn_rep.as_slice()).expect("decode child");
        proc.mark_exit(child.pid, 3).expect("exit");

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
        assert_eq!(waited.exit_code, 3);

        let mut vfs = VfsLiteService::new();
        let open = Message::with_header(
            0,
            VFS_OP_OPENAT,
            0,
            None,
            &VfsV1Args::new(0, 0x1000, 0, 0).encode(),
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
            &VfsV1Args::new(fd, 0x2000, 8, 0).encode(),
        )
        .expect("read");
        let read_rep = vfs.handle_request(read).expect("read rep");
        assert_eq!(read_rep.opcode, VFS_OP_READ);
    }
}
