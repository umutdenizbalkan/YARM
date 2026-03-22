#![no_std]
use yarm::kernel::ipc::Message;
use yarm::kernel::proc_abi::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args};
use yarm::kernel::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};
use yarm::kernel::vfs::{
    OpenAtRequest, ReadWriteRequest, VfsService, openat_message, read_message,
};

fn main() {
    let mut proc = ProcessService::new();
    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &SpawnV2Args::new(1, 99).encode(),
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
        &WaitPidV2Args::new(1, child.pid.0).encode(),
    )
    .expect("wait");
    let wait_rep = proc.handle(wait).expect("wait rep");
    let waited = WaitPidV2Result::decode(wait_rep.as_slice()).expect("waited");

    let mut vfs = VfsService::new();
    let open = openat_message(OpenAtRequest {
        dirfd: 0,
        path_ptr: 0x1000,
        flags: 0,
        mode: 0,
    })
    .expect("open");
    let open_rep = vfs.handle_request(open).expect("open rep");
    let mut fd_bytes = [0u8; 8];
    fd_bytes.copy_from_slice(open_rep.as_slice());
    let fd = u64::from_le_bytes(fd_bytes);

    let read = read_message(ReadWriteRequest {
        fd,
        buf_ptr: 0x2000,
        len: 16,
    })
    .expect("read");
    let _ = vfs.handle_request(read).expect("read rep");

    yarm::yarm_log!(
        "core profile smoke ok: child_pid={}, waited_exit={}, proc_handled={}",
        child.pid.0,
        waited.exit_code,
        proc.handled_count()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm::kernel::vfs_abi::VFS_OP_READ;

    #[test]
    fn core_profile_smoke_path_is_stable() {
        let mut proc = ProcessService::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 9).encode(),
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
            &WaitPidV2Args::new(1, child.pid.0).encode(),
        )
        .expect("wait");
        let wait_rep = proc.handle(wait).expect("wait rep");
        let waited = WaitPidV2Result::decode(wait_rep.as_slice()).expect("waited");
        assert_eq!(waited.exit_code, 3);

        let mut vfs = VfsService::new();
        let open = openat_message(OpenAtRequest {
            dirfd: 0,
            path_ptr: 0x1000,
            flags: 0,
            mode: 0,
        })
        .expect("open");
        let open_rep = vfs.handle_request(open).expect("open rep");
        let mut fd_bytes = [0u8; 8];
        fd_bytes.copy_from_slice(open_rep.as_slice());
        let fd = u64::from_le_bytes(fd_bytes);

        let read = read_message(ReadWriteRequest {
            fd,
            buf_ptr: 0x2000,
            len: 8,
        })
        .expect("read");
        let read_rep = vfs.handle_request(read).expect("read rep");
        assert_eq!(read_rep.opcode, VFS_OP_READ);
    }
}
