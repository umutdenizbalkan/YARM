// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_ipc_abi::blkcache_abi::*;
use yarm_ipc_abi::block_backend_abi::{
    BlkBackendQueryRequest, BlkBackendResponse, BLK_BACKEND_OP_QUERY_STATE,
};
use yarm_user_rt::ipc::Message;

#[derive(Clone, Copy)]
struct BackendRecord {
    backend_id: u64,
    backend_send_cap: u64,
    block_size: u32,
    flags: u32,
    block_count: u64,
    registered: bool,
}

impl BackendRecord {
    const fn empty() -> Self {
        Self { backend_id: 0, backend_send_cap: 0, block_size: 0, flags: 0, block_count: 0, registered: false }
    }
}

const MAX_BACKENDS: usize = 8;

fn return_reply(reply_cap: u32, opcode: u16, request_id: u64, status: u32) {
    let response = BlkCacheResponse { request_id, status, bytes_moved: 0, flags: 0 };
    if let Ok(reply) = Message::with_header(0, opcode, 0, None, &response.encode()) {
        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(reply_cap, &reply) };
    }
}

fn find_backend(table: &[BackendRecord; MAX_BACKENDS], backend_id: u64) -> Option<BackendRecord> {
    table.iter().copied().find(|r| r.registered && r.backend_id == backend_id)
}


fn probe_backend_query_state(backend_id: u64, backend_send_cap: u64, reply_recv_cap: u32) {
    yarm_user_rt::user_log!(
        "BLKCACHE_BACKEND_QUERY_STATE_BEGIN backend_id={}",
        backend_id
    );
    let req = BlkBackendQueryRequest {
        req_id: 0xB10C,
        flags: 0,
        device_id: backend_id,
    };
    let payload = req.encode();
    let Ok(msg) = Message::with_header(0, BLK_BACKEND_OP_QUERY_STATE, 0, None, &payload) else {
        yarm_user_rt::user_log!(
            "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err=BuildMessageFailed",
            backend_id
        );
        return;
    };
    match unsafe { yarm_user_rt::syscall::ipc_call(backend_send_cap as u32, reply_recv_cap, &msg) } {
        Ok(()) => {}
        Err(e) => {
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err={:?}",
                backend_id,
                e
            );
            return;
        }
    }
    match unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) } {
        Ok(Some(reply_msg)) => {
            if let Some(resp) = BlkBackendResponse::decode(reply_msg.as_slice()) {
                yarm_user_rt::user_log!(
                    "BLKCACHE_BACKEND_QUERY_STATE_RETURN backend_id={} status={} logical_block_size={} physical_block_size={}",
                    backend_id,
                    resp.status,
                    resp.logical_block_size,
                    resp.physical_block_size
                );
            }
        }
        Ok(None) => {
            yarm_user_rt::user_log!("BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err=NoReply", backend_id);
        }
        Err(e) => {
            yarm_user_rt::user_log!("BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err={:?}", backend_id, e);
        }
    }
}

fn register_backend(
    table: &mut [BackendRecord; MAX_BACKENDS],
    args: RegisterBackendArgs,
    transferred_backend_send_cap: u64,
) -> u32 {
    if args.backend_id == 0 || transferred_backend_send_cap == 0 || args.block_size == 0 {
        return BLKCACHE_STATUS_ERR_BAD_REQUEST;
    }
    for rec in table.iter() {
        if rec.registered && rec.backend_id == args.backend_id {
            return BLKCACHE_STATUS_ERR_BUSY;
        }
    }
    for rec in table.iter_mut() {
        if !rec.registered {
            *rec = BackendRecord {
                backend_id: args.backend_id,
                backend_send_cap: transferred_backend_send_cap,
                block_size: args.block_size,
                flags: args.flags,
                block_count: args.block_count,
                registered: true,
            };
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_REGISTERED backend_id={} block_size={}",
                args.backend_id,
                args.block_size
            );
            yarm_user_rt::user_log!(
                "BLKCACHE_BACKEND_REGISTER_CAP_TRANSFER backend_id={} cap={}",
                args.backend_id,
                transferred_backend_send_cap
            );
            return BLKCACHE_STATUS_OK;
        }
    }
    BLKCACHE_STATUS_ERR_NO_MEMORY
}

pub fn run() {
    yarm_user_rt::user_log!("BLKCACHE_SRV_ENTRY");
    let ctx = yarm_user_rt::runtime::startup_context();
    let pm_reply_recv_cap = ctx.process_manager_reply_recv_cap;
    if let Some(cap) = pm_reply_recv_cap {
        yarm_user_rt::user_log!("BLKCACHE_BACKEND_REPLY_RECV_CAP cap={}", cap);
    }
    let Some(recv_cap) = ctx.process_manager_service_recv_ep else {
        yarm_user_rt::user_log!("BLKCACHE_NO_RECV_CAP");
        loop { let _ = yarm_user_rt::syscall::yield_now(); }
    };
    yarm_user_rt::user_log!("BLKCACHE_SRV_RECV_CAP cap={}", recv_cap);
    yarm_user_rt::user_log!("BLKCACHE_BLOCKING_RECV_LOOP");

    let mut backends = [BackendRecord::empty(); MAX_BACKENDS];

    loop {
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some((msg, reply_cap))) => {
                let (request_id, status) = match msg.opcode {
                    BLKCACHE_OP_REGISTER_BACKEND => {
                        match RegisterBackendArgs::decode(msg.as_slice()) {
                            Some(args) => {
                                yarm_user_rt::user_log!("BLKCACHE_OP_REGISTER_BACKEND backend_id={}", args.backend_id);
                                yarm_user_rt::user_log!(
                                    "BLKCACHE_BACKEND_REGISTER_RECV transferred_cap={:?}",
                                    msg.transferred_cap().map(|c| c.0)
                                );
                                let Some(tx_cap) = msg.transferred_cap().map(|c| c.0) else {
                                    yarm_user_rt::user_log!(
                                        "BLKCACHE_BACKEND_QUERY_STATE_ERR backend_id={} err=MissingTransferredCap",
                                        args.backend_id
                                    );
                                    let status = BLKCACHE_STATUS_ERR_PERMISSION;
                                    if let Some(reply_cap) = reply_cap {
                                        return_reply(reply_cap, msg.opcode, 0, status);
                                    }
                                    continue;
                                };
                                {
                                    let status = register_backend(&mut backends, args, tx_cap);
                                    if status == BLKCACHE_STATUS_OK {
                                        yarm_user_rt::user_log!(
                                            "BLKCACHE_BACKEND_PROBE_AFTER_REGISTER backend_id={}",
                                            args.backend_id
                                        );
                                        if let Some(reply_cap) = pm_reply_recv_cap {
                                            if let Some(rec) = find_backend(&backends, args.backend_id) {
                                                probe_backend_query_state(rec.backend_id, rec.backend_send_cap, reply_cap);
                                            } else {
                                                yarm_user_rt::user_log!(
                                                    "BLKCACHE_BACKEND_QUERY_STATE_SKIP backend_id={} reason=BackendNotFoundAfterRegister",
                                                    args.backend_id
                                                );
                                            }
                                        } else {
                                            yarm_user_rt::user_log!(
                                                "BLKCACHE_BACKEND_QUERY_STATE_SKIP backend_id={} reason=NoReplyRecvCap",
                                                args.backend_id
                                            );
                                        }
                                    }
                                    if reply_cap.is_some() {
                                        (0, status)
                                    } else {
                                        continue;
                                    }
                                }
                            }
                            None => {
                                if reply_cap.is_some() {
                                    (0, BLKCACHE_STATUS_ERR_BAD_REQUEST)
                                } else {
                                    continue;
                                }
                            }
                        }
                    }
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
                if let Some(reply_cap) = reply_cap {
                    return_reply(reply_cap, msg.opcode, request_id, status);
                }
            }
            Ok(None) => {}
            Err(_e) => { let _ = yarm_user_rt::syscall::yield_now(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
fn register_backend_happy_and_duplicate() {
        let mut t = [BackendRecord::empty(); MAX_BACKENDS];
        let a = RegisterBackendArgs { backend_id: 1, backend_send_cap: 9, block_size: 512, flags: 0, block_count: 1 };
        assert_eq!(register_backend(&mut t, a, 9), BLKCACHE_STATUS_OK);
        assert_eq!(register_backend(&mut t, a, 9), BLKCACHE_STATUS_ERR_BUSY);
    }

    #[test]
    fn register_backend_invalid_and_capacity() {
        let mut t = [BackendRecord::empty(); MAX_BACKENDS];
        let bad = RegisterBackendArgs { backend_id: 0, backend_send_cap: 1, block_size: 512, flags: 0, block_count: 1 };
        assert_eq!(register_backend(&mut t, bad, 1), BLKCACHE_STATUS_ERR_BAD_REQUEST);
        for i in 0..MAX_BACKENDS {
            let ok = RegisterBackendArgs { backend_id: (i+1) as u64, backend_send_cap: (i+2) as u64, block_size: 512, flags: 0, block_count: 1 };
            assert_eq!(register_backend(&mut t, ok, (i+2) as u64), BLKCACHE_STATUS_OK);
        }
        let extra = RegisterBackendArgs { backend_id: 99, backend_send_cap: 77, block_size: 512, flags: 0, block_count: 1 };
        assert_eq!(register_backend(&mut t, extra, 77), BLKCACHE_STATUS_ERR_NO_MEMORY);
    }
}
