// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;
use yarm_ipc_abi::blkcache_abi::*;

pub fn run() {
    yarm_user_rt::user_log!("BLKCACHE_SRV_ENTRY");
    let ctx = yarm_user_rt::runtime::startup_context();
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("BLKCACHE_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("BLKCACHE_SRV_RECV_CAP cap={}", recv_cap);
    yarm_user_rt::user_log!("BLKCACHE_BLOCKING_RECV_LOOP");

    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some((msg, Some(reply_cap)))) => {
                let (request_id, status) = match msg.opcode {
                    BLKCACHE_OP_REGISTER_BACKEND => { yarm_user_rt::user_log!("BLKCACHE_OP_REGISTER_BACKEND"); RegisterBackendArgs::decode(msg.as_slice()).map(|a|(a.backend_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_REGISTER_BUFFER => { yarm_user_rt::user_log!("BLKCACHE_OP_REGISTER_BUFFER"); RegisterBufferArgs::decode(msg.as_slice()).map(|a|(a.buffer_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_UNREGISTER_BUFFER => { yarm_user_rt::user_log!("BLKCACHE_OP_UNREGISTER_BUFFER"); UnregisterBufferArgs::decode(msg.as_slice()).map(|a|(a.buffer_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_READ_BLOCK => { yarm_user_rt::user_log!("BLKCACHE_OP_READ_BLOCK"); BlockIoRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_WRITE_BLOCK => { yarm_user_rt::user_log!("BLKCACHE_OP_WRITE_BLOCK"); BlockIoRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_FLUSH => { yarm_user_rt::user_log!("BLKCACHE_OP_FLUSH"); RangeRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_INVALIDATE => { yarm_user_rt::user_log!("BLKCACHE_OP_INVALIDATE"); RangeRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_GET_STATS => { yarm_user_rt::user_log!("BLKCACHE_OP_GET_STATS"); GetStatsRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    BLKCACHE_OP_CANCEL => { yarm_user_rt::user_log!("BLKCACHE_OP_CANCEL"); CancelRequest::decode(msg.as_slice()).map(|a|(a.request_id,BLKCACHE_STATUS_ERR_UNSUPPORTED)).unwrap_or((0,BLKCACHE_STATUS_ERR_BAD_REQUEST)) }
                    _ => (0, BLKCACHE_STATUS_ERR_BAD_REQUEST),
                };
                let response = BlkCacheResponse { request_id, status, bytes_moved: 0, flags: 0 };
                let reply = Message::with_header(0, msg.opcode, 0, None, &response.encode())
                    .unwrap_or_else(|_| Message::new(1, &[]).expect("blkcache unsupported reply"));
                let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
            }
            Ok(Some((_msg, None))) => {}
            Ok(None) => {}
            Err(_e) => {
                let _ = yarm_user_rt::syscall::yield_now();
            }
        }
    }
}
