use crate::kernel::ipc::Message;
use crate::kernel::proc_abi::{SpawnV2Args, WaitPidV2Args, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2};
use crate::kernel::process_manager::{ProcessService, SpawnV2Result, WaitPidV2Result};
use crate::services::common::service::{run_typed_request_loop, RequestResponseService};

impl RequestResponseService for ProcessService {
    type Error = crate::kernel::process_manager::ProcessManagerError;

    fn service_name(&self) -> &'static str {
        "procman"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        ProcessService::handle(self, request)
    }
}

pub fn run() {
    let mut service = ProcessService::new();

    let replies = run_typed_request_loop(
        &mut service,
        [Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 42).encode(),
        )
        .expect("spawn")],
    )
    .expect("spawn loop");
    let spawn_reply = replies[0];
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("spawn decode");

    service.mark_exit(spawned.pid, 0).expect("exit");

    let wait_reply = run_typed_request_loop(
        &mut service,
        [Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait")],
    )
    .expect("wait loop")[0];
    let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("wait decode");

    crate::yarm_log!(
        "process-manager demo ready: pid={}, exit_code={}, handled={}",
        waited.waited_pid.0,
        waited.exit_code,
        service.handled_count()
    );
}
