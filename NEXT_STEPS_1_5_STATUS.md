# Recommended Next Steps 1-5 Status

1. **Process-manager protocol v2 baseline**: added dual-argument process-manager request support (`send_linux_process_manager_request2`), frozen v2 opcodes (`PROC_OP_SPAWN_V2`, `PROC_OP_WAITPID_V2`), explicit codecs (`ProcV2Args`, `SpawnV2Result`, `WaitPidV2Result`), and a minimal `process_manager` service with typed request parsing.
2. **IOVA/DMA revoke semantics**: added explicit `detach_driver_iova_space` and `revoke_driver_runtime_caps` lifecycle hooks; runtime driver caps are now revoked from cspace on restart/dead transitions and covered by stale-cap tests.
3. **Supervisor + restart policy hooks**: added class-based restart policy defaults (`App`, `Driver`, `SystemServer`), class-specific escalation thresholds (`set_class_escalation_threshold`), and policy observability (`ClassPolicySnapshot`).
4. **VFS IPC contract depth**: extended VFS syscall routing assertions and split VFS-lite into parser + backend traits (`VfsRequest`, `VfsBackend`, `InMemoryBackend`) with mount-router routing support.
5. **Vertical server-path hardening**: added an end-to-end Linux personality shim coverage set including deterministic mixed `getpid/openat/exit` routing across process-manager and VFS manager IPC.
