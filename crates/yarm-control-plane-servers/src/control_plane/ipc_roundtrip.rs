// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm::kernel::boot::{KernelError, KernelState};
use yarm::kernel::capabilities::CapId;
use yarm::kernel::ipc::{Message, ThreadId};
use yarm::services::common::service::RequestResponseService;

pub fn roundtrip_call_reply_with_budget<S, E, FKernel, FMalformed, FMissingTid>(
    kernel: &mut KernelState,
    service: &mut S,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    client_recv_cap: CapId,
    request: Message,
    recv_timeout_ticks: u64,
    map_kernel_err: FKernel,
    malformed_err: FMalformed,
    missing_tid_err: FMissingTid,
) -> Result<Message, E>
where
    S: RequestResponseService<Error = E>,
    FKernel: Fn(KernelError) -> E + Copy,
    FMalformed: Fn() -> E + Copy,
    FMissingTid: Fn() -> E + Copy,
{
    let caller_tid = ThreadId(kernel.current_tid().ok_or_else(missing_tid_err)?);
    let reply_cap = kernel
        .create_reply_cap_for_caller(caller_tid, client_recv_cap, None)
        .map_err(map_kernel_err)?;
    let request_with_reply_cap = Message::with_header(
        request.sender_tid.0,
        request.opcode,
        request.flags | Message::FLAG_CAP_TRANSFER,
        Some(reply_cap.0),
        request.as_slice(),
    )
    .map_err(|_| malformed_err())?;

    kernel
        .ipc_send(client_send_cap, request_with_reply_cap)
        .map_err(map_kernel_err)?;
    let request_for_server = kernel
        .ipc_recv_with_deadline(server_recv_cap, recv_timeout_ticks)
        .map_err(map_kernel_err)?
        .ok_or_else(malformed_err)?;
    let reply_cap = request_for_server
        .transferred_cap()
        .map(|cap| CapId(cap.0))
        .ok_or_else(malformed_err)?;
    let sanitized_request = Message::with_header(
        request_for_server.sender_tid.0,
        request_for_server.opcode,
        request_for_server.flags & !Message::FLAG_CAP_TRANSFER,
        None,
        request_for_server.as_slice(),
    )
    .map_err(|_| malformed_err())?;
    let response = service.handle(sanitized_request)?;
    kernel
        .ipc_reply(reply_cap, response)
        .map_err(map_kernel_err)?;
    kernel
        .ipc_recv_with_deadline(client_recv_cap, recv_timeout_ticks)
        .map_err(map_kernel_err)?
        .ok_or_else(malformed_err)
}
