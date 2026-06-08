// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[path = "sysdeps/kernel_hooks.rs"]
mod kernel_hooks;
#[path = "sysdeps/musl_startup.rs"]
mod musl_startup;
#[path = "sysdeps/service_hooks.rs"]
mod service_hooks;

pub use kernel_hooks::*;
pub use musl_startup::*;
pub use service_hooks::*;

#[cfg(test)]
mod tests {
    #[test]
    fn service_hooks_are_ipc_boundary_oriented_not_legacy_in_process_services() {
        let src = include_str!("sysdeps/service_hooks.rs");
        let legacy_proc = ["crate::kernel::process::", "ProcessService"].concat();
        let legacy_fs = ["crate::service_common::service::", "FsService"].concat();
        let legacy_socket = [
            "crate::yarm_network_servers::socket::service::",
            "SocketAdapterService",
        ]
        .concat();
        assert!(
            !src.contains(legacy_proc.as_str()),
            "posix service hooks should not couple to in-process ProcessService"
        );
        assert!(
            !src.contains(legacy_fs.as_str()),
            "posix service hooks should not couple to in-process FsService"
        );
        assert!(
            !src.contains(legacy_socket.as_str()),
            "posix service hooks should not couple to in-process SocketAdapterService"
        );
        assert!(
            src.contains("PosixServiceBindings"),
            "posix service hooks should rely on binding-based IPC boundary client path"
        );
        assert!(
            src.contains("dispatch("),
            "posix service hooks should route requests through syscall dispatch boundary"
        );
        assert!(
            src.contains("LINUX_NR_SOCKET"),
            "posix service hooks should route socket compatibility through syscall dispatch boundary"
        );
        assert!(
            src.contains("LINUX_NR_CONNECT"),
            "posix service hooks should route connect compatibility through syscall dispatch boundary"
        );
        assert!(
            src.contains("LINUX_NR_SENDTO"),
            "posix service hooks should route sendto compatibility through syscall dispatch boundary"
        );
        assert!(
            src.contains("SOCKET_OP_SOCKET"),
            "posix service hooks should consume shared socket ABI opcode contract"
        );
        assert!(
            src.contains("SOCKET_OP_CONNECT"),
            "posix service hooks should consume shared connect ABI opcode contract"
        );
        assert!(
            src.contains("SOCKET_OP_SENDTO"),
            "posix service hooks should consume shared sendto ABI opcode contract"
        );
        assert!(
            !src.contains("Err(PosixErrno::NoSys)"),
            "socket hook should no longer be a NoSys placeholder in service hooks"
        );
    }
}
