# AArch64 IPC/bootstrap status (current)

## Summary

The current AArch64 bring-up behavior after core bootstrap is **expected steady-state idle**:

- `init_server` sends one spawn request to process_manager, then blocks on `init_alert_recv_ep`.
- `process_manager` remains a long-lived server and blocks waiting for additional requests.

As a result, repeated wake/sleep at the recv-wait PCs for `tid=1` (init) and `tid=3` (process_manager) is quiescent runtime behavior, not a crash loop.

## Fixed issues in this sequence

1. IPC recv return/resume path no longer corrupts reply-cap propagation.
2. PM reply path now uses the correct reply capability (`reply_cap=65537` in observed logs).
3. `ipc_recv_v2` keeps reply-cap separate from `Message` transferred-cap metadata.
4. `ipc_call` AArch64/RISC-V success decode no longer treats any non-zero `ret0` as automatic failure.
5. Trapframe user context capture/restore preserves full architectural user registers (no x0..x5 clobber from syscall scratch args).
6. init no longer falls through into entrypoint tail loop after the first PM reply.
7. init emits a durable wait marker before entering alert receive wait:
   - `INIT_ALERT_WAIT_BEGIN cap=<...>`

## Operational expectation

With no additional alerts or control-plane requests injected, the system should remain in this recv-wait quiescent state.
