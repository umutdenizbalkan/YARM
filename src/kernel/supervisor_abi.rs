// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::ipc::Message;

pub use yarm_ipc_abi::supervisor_abi::*;

pub fn task_exited_message(sender_tid: u64, event: TaskExitedEvent) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_TASK_EXITED,
        0,
        None,
        &event.encode(),
    )
    .map_err(|_| ())
}

pub fn transfer_revoked_message(
    sender_tid: u64,
    event: TransferRevokedEvent,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_TRANSFER_REVOKED,
        0,
        None,
        &event.encode(),
    )
    .map_err(|_| ())
}

pub fn init_alert_message(sender_tid: u64, alert: InitAlert) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_INIT_ALERT,
        0,
        None,
        &alert.encode(),
    )
    .map_err(|_| ())
}

pub fn register_core_service_message(
    sender_tid: u64,
    request: RegisterCoreServiceRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_REGISTER_CORE_SERVICE,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

pub fn register_driver_message(
    sender_tid: u64,
    request: RegisterDriverRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_REGISTER_DRIVER,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

pub fn query_status_message(
    sender_tid: u64,
    request: SupervisorStatusRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

pub fn redelegation_ack_message(
    sender_tid: u64,
    request: RedelegationAckRequest,
) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_ACK_REDELEGATION,
        0,
        None,
        &request.encode(),
    )
    .map_err(|_| ())
}

pub fn status_reply_message(sender_tid: u64, reply: SupervisorStatusReply) -> Result<Message, ()> {
    Message::with_header(
        sender_tid,
        SUPERVISOR_OP_QUERY_STATUS,
        0,
        None,
        &reply.encode(),
    )
    .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_exited_message_uses_supervisor_opcode() {
        let msg = task_exited_message(
            7,
            TaskExitedEvent {
                tid: 1,
                exit_code: 2,
                restart_token: 3,
            },
        )
        .expect("msg");
        assert_eq!(msg.opcode, SUPERVISOR_OP_TASK_EXITED);
    }

    #[test]
    fn status_reply_roundtrip_is_stable() {
        let reply = SupervisorStatusReply {
            tid: 9,
            degraded: true,
            pending_redelegation: false,
            restart_attempts: 2,
            restart_group: 1,
            max_restarts: 5,
            restart_owner: 3,
            last_exit_code: 11,
            last_exit_tick: 12,
            pending_restart_due: 13,
            last_restart_tick: 14,
        };
        assert_eq!(SupervisorStatusReply::decode(&reply.encode()), Some(reply));
    }
}
