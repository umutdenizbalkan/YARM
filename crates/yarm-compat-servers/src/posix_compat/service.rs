// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use yarm_user_rt::syscall::SyscallIpcTransport;

pub fn run() {
    let startup = yarm_user_rt::runtime::startup_context();
    let mut transport = SyscallIpcTransport;
    let mut sysdeps = super::sysdeps::PosixSysdepsContext::new(&mut transport);

    let registration_result = startup
        .process_manager_caps()
        .map(|(request_send, reply_recv)| {
            crate::yarm_log!(
                "POSIX_COMPAT_PM_CAPS_READY task_id={} req={} reply={}",
                startup.task_id,
                request_send,
                reply_recv
            );
            sysdeps.register_process_manager(request_send, reply_recv)
        });
    let getpid_ipc_ready = matches!(registration_result, Some(Ok(())));

    crate::yarm_log!(
        "posix-compat server startup: task_id={}, proc_req_cap_present={}, proc_rep_cap_present={}, getpid_ipc_ready={}",
        startup.task_id,
        startup.process_manager_request_send_cap.is_some(),
        startup.process_manager_reply_recv_cap.is_some(),
        getpid_ipc_ready
    );

    if let Some(Err(err)) = registration_result {
        crate::yarm_log!(
            "posix-compat startup: process-manager cap registration failed ({:?}); getpid remains graceful NoSys",
            err
        );
    } else if !getpid_ipc_ready {
        crate::yarm_log!("POSIX_COMPAT_PM_CAPS_MISSING task_id={}", startup.task_id);
        crate::yarm_log!(
            "posix-compat startup: process-manager caps missing from startup_context; getpid remains graceful NoSys"
        );
    }
}
