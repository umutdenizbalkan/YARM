// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::runtime::StartupContext;
use yarm_user_rt::syscall::SyscallIpcTransport;

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
        // Runtime endpoint-cap extraction is not wired yet; keep explicit None
        // until startup context exposes process-manager IPC caps.
        Self::default()
    }

    #[inline]
    const fn process_manager_caps(self) -> Option<(u32, u32)> {
        match (self.proc_mgr_request_send, self.proc_mgr_reply_recv) {
            (Some(request_send), Some(reply_recv)) => Some((request_send, reply_recv)),
            _ => None,
        }
    }
}

pub fn run() {
    let startup = yarm_user_rt::runtime::startup_context();
    let handoff = PosixRuntimeHandoff::from_startup_context(startup);
    let mut transport = SyscallIpcTransport;
    let mut sysdeps = super::sysdeps::PosixSysdepsContext::new(&mut transport);

    let registration_result = handoff
        .process_manager_caps()
        .map(|(request_send, reply_recv)| {
            sysdeps.register_process_manager(request_send, reply_recv)
        });
    let getpid_ipc_ready = matches!(registration_result, Some(Ok(())));

    crate::yarm_log!(
        "posix-compat server startup: task_id={}, proc_req_cap_present={}, proc_rep_cap_present={}, getpid_ipc_ready={}",
        startup.task_id,
        handoff.proc_mgr_request_send.is_some(),
        handoff.proc_mgr_reply_recv.is_some(),
        getpid_ipc_ready
    );

    if let Some(Err(err)) = registration_result {
        crate::yarm_log!(
            "posix-compat startup: process-manager cap registration failed ({:?}); getpid remains graceful NoSys",
            err
        );
    } else if !getpid_ipc_ready {
        crate::yarm_log!(
            "posix-compat startup: process-manager caps missing from startup_context; getpid remains graceful NoSys"
        );
    }
}
