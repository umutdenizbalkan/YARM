extern crate std;

use std::println;

use crate::kernel::ipc::Message;
use crate::kernel::proc_proto::{PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, ProcV2Args};
use crate::kernel::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};

pub fn run() {
    let mut service = ProcessService::new();

    let spawn = Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &ProcV2Args::new(1, 42).encode(),
    )
    .expect("spawn");
    let spawn_reply = service.handle(spawn).expect("spawn reply");
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("spawn decode");

    service.mark_exit(spawned.pid, 0).expect("exit");

    let wait = Message::with_header(
        0,
        PROC_OP_WAITPID_V2,
        0,
        None,
        &ProcV2Args::new(1, spawned.pid).encode(),
    )
    .expect("wait");
    let wait_reply = service.handle(wait).expect("wait reply");
    let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("wait decode");

    println!(
        "process-manager demo ready: pid={}, exit_code={}, handled={}",
        waited.waited_pid,
        waited.exit_code,
        service.handled_count()
    );
}
