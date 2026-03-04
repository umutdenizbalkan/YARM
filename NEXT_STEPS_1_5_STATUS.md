# Recommended Next Steps 1-5 Status

1. **Process-manager protocol v2 baseline**: added dual-argument process-manager request support (`send_linux_process_manager_request2`), frozen v2 opcodes (`PROC_OP_SPAWN_V2`, `PROC_OP_WAITPID_V2`), and an explicit `ProcV2Args` codec with round-trip tests.
2. **IOVA/DMA revoke semantics**: added explicit `detach_driver_iova_space` and `revoke_driver_runtime_caps` lifecycle hooks, and now revoke driver runtime grants on restart/dead transitions.
3. **Supervisor + restart policy hooks**: added class-based restart policy defaults (`App`, `Driver`, `SystemServer`) and class-specific escalation threshold controls (`set_class_restart_policy`, `set_class_escalation_threshold`, `register_task_with_class`).
4. **VFS IPC contract depth**: extended VFS syscall routing tests to validate packed payload boundaries/field slots for `epoll_ctl`, `sendfile`, and `statx` message contracts.
5. **Vertical server-path hardening**: introduced a minimal `vfs_lite` service module with `open/read/write/close/statx` handlers and a demo binary path that exercises Linux-compat-to-VFS reply wiring.
