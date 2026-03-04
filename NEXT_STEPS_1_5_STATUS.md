# Recommended Next Steps 1-5 Status

1. **Process-manager protocol v2 baseline**: added dual-argument process-manager request support (`send_linux_process_manager_request2`), frozen v2 opcodes (`PROC_OP_SPAWN_V2`, `PROC_OP_WAITPID_V2`), an explicit `ProcV2Args` codec, and a minimal `process_manager` service module with typed `SpawnV2Request`/`WaitPidV2Request` parsing.
2. **IOVA/DMA revoke semantics**: added explicit `detach_driver_iova_space` and `revoke_driver_runtime_caps` lifecycle hooks, now revoking driver runtime caps from cspace on restart/dead transitions.
3. **Supervisor + restart policy hooks**: added class-based restart policy defaults (`App`, `Driver`, `SystemServer`), class-specific escalation threshold controls (`set_class_restart_policy`, `set_class_escalation_threshold`, `register_task_with_class`), and class policy snapshot reporting.
4. **VFS IPC contract depth**: extended VFS syscall routing tests to validate packed payload boundaries/field slots for `epoll_ctl`, `sendfile`, and `statx` message contracts, and split VFS-lite into parser + backend trait abstractions.
5. **Vertical server-path hardening**: added minimal `vfs_lite` and `process_manager` service modules and a Linux personality shim end-to-end test covering `getpid` + `openat` + `exit` server routing.
