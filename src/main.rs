// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
extern crate std;

use yarm::kernel::boot::Bootstrap;
use yarm::kernel::ipc::Message;
use yarm::kernel::syscall::{SYSCALL_NO_TRANSFER_CAP, Syscall};
use yarm::kernel::trap::Trap;
use yarm::kernel::trapframe::TrapFrame;

use std::println;

fn main() {
    let mut kernel = Bootstrap::init().expect("bootstrap failed");
    let (_endpoint, send_cap, recv_cap) =
        kernel.create_endpoint(4).expect("endpoint create failed");

    kernel
        .ipc_send(
            send_cap,
            Message::new(1, b"boot-msg").expect("message build failed"),
        )
        .expect("ipc send failed");
    let _ = kernel.ipc_recv(recv_cap).expect("ipc recv failed");

    let send_payload = usize::from_le_bytes([b'o', b'k', 0, 0, 0, 0, 0, 0]);
    let mut send_tf = TrapFrame::new(
        Syscall::IpcSend as usize,
        [
            send_cap.0 as usize,
            1,
            2,
            send_payload,
            0,
            SYSCALL_NO_TRANSFER_CAP as usize,
        ],
    );
    kernel
        .handle_trap(Trap::Syscall, Some(&mut send_tf))
        .expect("syscall send trap failed");

    let mut recv_tf = TrapFrame::new(
        Syscall::IpcRecv as usize,
        [recv_cap.0 as usize, 0, 0, 0, 0, 0],
    );
    kernel
        .handle_trap(Trap::Syscall, Some(&mut recv_tf))
        .expect("syscall recv trap failed");

    kernel
        .handle_trap(Trap::TimerInterrupt, None)
        .expect("timer trap failed");

    println!(
        "YARM core online: mappings={}, runnable_tasks={}, current={}",
        kernel.kernel_aspace.mappings(),
        kernel.scheduler.runnable_count(),
        kernel.current_tid().unwrap_or(0)
    );
}
