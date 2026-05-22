// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::block_backend_abi::*;
use yarm_user_rt::ipc::Message;

fn decode_query_request(msg: &Message) -> Result<BlkBackendQueryRequest, i32> {
    BlkBackendQueryRequest::decode(msg.as_slice()).ok_or(BLK_BACKEND_STATUS_EINVAL)
}

fn decode_io_request(msg: &Message) -> Result<BlkBackendRequest, i32> {
    let req = BlkBackendRequest::decode(msg.as_slice()).ok_or(BLK_BACKEND_STATUS_EINVAL)?;
    if !req.is_valid_for_opcode(msg.opcode) {
        return Err(BLK_BACKEND_STATUS_EINVAL);
    }
    Ok(req)
}

fn build_resp(req_id: u32, status: i32) -> BlkBackendResponse {
    BlkBackendResponse {
        req_id,
        status,
        actual_bytes: 0,
        backend_generation: 0,
        logical_block_size: 512,
        physical_block_size: 512,
    }
}

pub fn run() {
    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        return;
    };
    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some((msg, Some(reply_cap)))) => {
                let (req_id, status) = match msg.opcode {
                    BLK_BACKEND_OP_QUERY_STATE => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_QUERY_STATE");
                        match decode_query_request(&msg) { Ok(req) => (req.req_id, BLK_BACKEND_STATUS_EAGAIN), Err(e) => (0, e) }
                    }
                    BLK_BACKEND_OP_READ => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_READ");
                        match decode_io_request(&msg) { Ok(req) => (req.req_id, BLK_BACKEND_STATUS_ENOSYS), Err(e) => (0, e) }
                    }
                    BLK_BACKEND_OP_WRITE => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_WRITE");
                        match decode_io_request(&msg) { Ok(req) => (req.req_id, BLK_BACKEND_STATUS_ENOSYS), Err(e) => (0, e) }
                    }
                    BLK_BACKEND_OP_FLUSH => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_FLUSH");
                        match decode_io_request(&msg) { Ok(req) => (req.req_id, BLK_BACKEND_STATUS_ENOSYS), Err(e) => (0, e) }
                    }
                    BLK_BACKEND_OP_GET_GEOM => {
                        yarm_user_rt::user_log!("VIRTIO_BLK_OP_GET_GEOM");
                        match decode_query_request(&msg) { Ok(req) => (req.req_id, BLK_BACKEND_STATUS_EAGAIN), Err(e) => (0, e) }
                    }
                    _ => (0, BLK_BACKEND_STATUS_EINVAL),
                };
                let resp = build_resp(req_id, status);
                if let Ok(reply) = Message::with_header(0, msg.opcode, 0, None, &resp.encode()) {
                    let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
                }
            }
            Ok(Some((_msg, None))) => {}
            Ok(None) => {}
            Err(_) => { let _ = yarm_user_rt::syscall::yield_now(); }
        }
    }
}
