// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::ipc::Message;
use crate::kernel::process::ProcessManagerError;
use crate::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use crate::kernel::process_abi::{
    PROC_OP_EXIT, PROC_OP_SPAWN_V2, PROC_OP_WAITPID_V2, SpawnV2Args, WaitPidV2Args,
};
use crate::services::common::service::{RequestResponseService, run_typed_request_loop};

impl RequestResponseService for ProcessService {
    type Error = crate::kernel::process::ProcessManagerError;

    fn service_name(&self) -> &'static str {
        "process_manager"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        ProcessService::handle(self, request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessManagerLoopSummary {
    pub spawned_pid: u64,
    pub waited_pid: u64,
    pub waited_exit: u64,
    pub handled: usize,
}

pub fn run_request_loop(
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    let replies = run_typed_request_loop(
        service,
        [Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(parent_pid, image_id).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?;
    let spawn_reply = replies[0];
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice())?;

    let _ = run_typed_request_loop(
        service,
        [Message::with_header(
            spawned.pid.0,
            PROC_OP_EXIT,
            0,
            None,
            &exit_code.to_le_bytes(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?;

    let wait_reply = run_typed_request_loop(
        service,
        [Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(parent_pid, spawned.pid.0).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?[0];
    let waited = WaitPidV2Result::decode(wait_reply.as_slice())?;

    Ok(ProcessManagerLoopSummary {
        spawned_pid: spawned.pid.0,
        waited_pid: waited.waited_pid.0,
        waited_exit: waited.exit_code,
        handled: service.handled_count(),
    })
}

pub fn run() {
    let mut service = ProcessService::new();
    let summary = run_request_loop(&mut service, 1, 42, 0).expect("process-manager loop");

    crate::yarm_log!(
        "process-manager request-loop ready: pid={}, exit_code={}, handled={}",
        summary.waited_pid,
        summary.waited_exit,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_manager_request_loop_entrypoint_runs_spawn_and_wait() {
        let mut service = ProcessService::new();
        let summary = run_request_loop(&mut service, 7, 42, 9).expect("loop");

        assert_eq!(summary.spawned_pid, summary.waited_pid);
        assert_eq!(summary.waited_exit, 9);
        assert_eq!(summary.handled, 3);
    }
}
