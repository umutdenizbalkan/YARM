// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::runtime::StartupContext;
use yarm_user_rt::syscall::{IpcTransport, SyscallIpcTransport};

/// Minimal runtime handoff for normal userspace startup.
///
/// This intentionally avoids any kernel-internal bootstrap/state usage and can
/// be replaced with real boot/runtime arguments when those are wired through
/// server startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PosixRuntimeHandoff {
    proc_mgr_request_send: Option<u32>,
    proc_mgr_reply_recv: Option<u32>,
}

impl PosixRuntimeHandoff {
    #[inline]
    fn from_startup_context(_ctx: StartupContext) -> Self {
        // No runtime-provided endpoint caps are plumbed yet.
        Self::default()
    }
}

pub fn run() {
    let startup = yarm_user_rt::runtime::startup_context();
    let handoff = PosixRuntimeHandoff::from_startup_context(startup);
    let mut transport = SyscallIpcTransport;

    // If/when startup provides endpoint caps, probe the reply channel using the
    // userspace syscall IPC transport. Until then, startup remains a no-kernel
    // runtime handoff stub.
    let routed_reply = handoff
        .proc_mgr_reply_recv
        .and_then(|recv_cap| transport.recv(recv_cap).ok())
        .flatten()
        .is_some();

    crate::yarm_log!(
        "posix-compat server startup: task_id={}, proc_req_cap_present={}, routed_reply={}",
        startup.task_id,
        handoff.proc_mgr_request_send.is_some(),
        routed_reply
    );
}
