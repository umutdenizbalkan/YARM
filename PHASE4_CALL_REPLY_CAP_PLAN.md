<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 4 Call/Reply Capability Plan

This document defines the implementation slices for seL4-style reply-cap IPC.

## Scope for Phase 4

- Add an explicit call primitive (`IpcCall`) that mints an ephemeral reply capability.
- Bind reply capability to caller and invocation context.
- Ensure single-use semantics and revoke-on-use behavior.
- Reject replay/use-after-consume attempts deterministically.

## Planned slices

1. **Kernel object model extension** ✅ implemented
   - Add `CapObject::Reply` with generation-protected slot identity.
   - Add bounded in-kernel reply-cap record table with owner/caller/endpoint binding metadata.

2. **Call path integration** ⏳ pending
   - Add syscall ABI entry for `IpcCall`.
   - During call send path, create ephemeral reply capability and transfer it to callee.

3. **Reply path integration** 🟡 partially implemented
   - Add syscall ABI entry for `IpcReply`.
   - Resolve reply cap, deliver message to bound caller endpoint, invalidate reply cap record atomically.

4. **Hardening** ⏳ pending
   - Prevent cap reuse after consume.
   - Revoke orphaned reply caps on caller exit/restart.
   - Bind reply cap use to intended caller endpoint context.

## Test matrix

- call->reply success roundtrip
- reply cap single-use rejection on second attempt
- wrong-task reply cap use rejection
- caller exit cleanup revokes pending reply caps
- replay attempts with stale generation fail
