<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 4 Call/Reply Capability Plan

This document defines the implementation slices for seL4-style reply-cap IPC.

> **Historical/staged design note.**
> The authoritative current ABI/behavior is documented in:
> - `doc/SYSCALL_ABI.md`
> - `doc/AARCH64_IPC_VFS_PM_STATUS_2026_05.md`
>
> This plan remains useful for implementation history; where statements differ, follow the two documents above.

## Scope for Phase 4

- Add an explicit call primitive (`IpcCall`) that mints an ephemeral reply capability.
- Bind reply capability to caller and invocation context.
- Ensure single-use semantics and revoke-on-use behavior.
- Reject replay/use-after-consume attempts deterministically.

## Planned slices

1. **Kernel object model extension** ✅ implemented
   - Add `CapObject::Reply` with generation-protected slot identity.
   - Add bounded in-kernel reply-cap record table with owner/caller/endpoint binding metadata.

2. **Call path integration** 🟡 partially implemented
   - Add syscall ABI entry for `IpcCall`.
   - During call send path, create ephemeral reply capability and transfer it to callee.
   - Superseded clarification: `IpcCall` is send/queue-only; callers receive replies explicitly via `IpcRecv`/`IpcRecvTimeout` (recv-v2 out-meta path), not inline in return registers.

3. **Reply path integration** 🟡 partially implemented
   - Add syscall ABI entry for `IpcReply`.
   - Resolve reply cap, deliver message to bound caller endpoint, invalidate reply cap record atomically.
   - Superseded clarification: blocked recv-v2 waiters are completed at delivery time (payload + out-meta copy, wake with syscall success), without syscall replay.

4. **Hardening** 🟡 in progress
   - Prevent cap reuse after consume.
   - Revoke orphaned reply caps on caller exit/restart. ✅ caller exit/reap/restart revocation added.
   - Bind reply cap use to intended caller endpoint context. ✅ call-minted reply caps now bind expected responder task.
   - Superseded clarification: userspace sees receiver-local materialized cap IDs only; raw reply handles are never exposed, and one delivered message cannot be re-received to rematerialize a second cap.

## Test matrix

- call->reply success roundtrip
- reply cap single-use rejection on second attempt
- wrong-task reply cap use rejection
- caller exit cleanup revokes pending reply caps
- replay attempts with stale generation fail
