// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::ipc::Message;

const BLKCACHE_STATUS_UNSUPPORTED: u64 = 1;

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
                let reply = Message::with_header(0, msg.opcode, 0, None, &BLKCACHE_STATUS_UNSUPPORTED.to_le_bytes())
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
