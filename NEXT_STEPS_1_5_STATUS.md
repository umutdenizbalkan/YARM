# Recommended Next Steps 1-5 Status

1. **Process-manager protocol v2 baseline**: added dual-argument process-manager request support (`send_linux_process_manager_request2`) and frozen v2 opcodes (`PROC_OP_SPAWN_V2`, `PROC_OP_WAITPID_V2`) with round-trip tests for payload sizing and reply routing.
2. **IOVA/DMA revoke semantics**: added explicit `detach_driver_iova_space` and `revoke_driver_runtime_caps` lifecycle hooks, and now revoke driver runtime grants on restart/dead transitions.
3. **Supervisor + restart policy hooks**: added class-based restart policy defaults (`App`, `Driver`, `SystemServer`) with `set_class_restart_policy` and `register_task_with_class`, plus verification that class policy is applied at task registration.
4. **VFS IPC contract depth**: extended VFS syscall routing tests to validate packed payload boundaries/field slots for `epoll_ctl`, `sendfile`, and `statx` message contracts.
5. **Vertical server-path hardening**: continued mechanism-only vertical wiring with stronger process/VFS IPC contract checks and driver-manager lifecycle safety under restart transitions.
