// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::Bootstrap;
use crate::kernel::ipc::Message;
use crate::kernel::trapframe::TrapFrame;
use yarm_ipc_abi::process_abi::PROC_OP_GETPID;

use super::{LINUX_NR_GETPID, PosixServiceBindings, dispatch};

pub fn run() {
    let mut kernel = Bootstrap::init().expect("init");
    let mut bindings = PosixServiceBindings::default();

    let (_proc_req_ep, proc_req_send, proc_req_recv) = kernel.create_endpoint(8).expect("proc req");
    let (_proc_rep_ep, proc_rep_send, proc_rep_recv) = kernel.create_endpoint(8).expect("proc rep");
    bindings
        .register_process_manager(&kernel, proc_req_send, proc_rep_recv)
        .expect("bind proc");

    kernel
        .ipc_send(
            proc_rep_send,
            Message::with_header(0, PROC_OP_GETPID, 0, None, &42u64.to_le_bytes()).expect("reply"),
        )
        .expect("seed reply");

    let mut frame = TrapFrame::new(LINUX_NR_GETPID, [0, 0, 0, 0, 0, 0]);
    dispatch(&mut kernel, &bindings, &mut frame);

    let routed = kernel.ipc_recv(proc_req_recv).expect("recv").is_some();
    crate::yarm_log!(
        "posix-compat server demo: translated getpid -> ret={}, routed_request={}",
        frame.ret0(),
        routed
    );
}
