<!-- SPDX-License-Identifier: Apache-2.0 -->

# IPC v2 Shared-Reply Adoption Checklist (Stage 1)

This checklist is for services adopting the **IPC v2 shared-reply convention**:

- server replies with `REPLY_V2 + TRANSFER_CAP`
- reply payload is `IpcV2SharedReplyMeta`
- caller receives `ret_transfer_cap` and maps explicitly later
- no automatic mapping

## Current migration status snapshot

- IPC v1 user-runtime API removed.
- IPC v1 kernel syscall slots are reserved / return `InvalidNumber`.
- IPC v2 `SEND/RECV/CALL/REPLY` paths are active.
- Timeout receive parity is implemented.
- Large-reply copyout and `BufferTooSmall` behavior are implemented.
- Supervisor traffic uses mandatory opcode envelopes.
- Shared-reply Stage 1 is implemented and hardened:
  - transfer cap must be `MemoryObject` when payload is valid shared-reply metadata;
  - metadata bounds (`offset + len`) must fit the transferred `MemoryObject` length.
- First service adoption target selected: **VFS `READ` reply path**.
- Pass 2 status: VFS producer wiring uses staged `VmAnonMap` + shared metadata/cap transfer path with producer-local `VmUnmap` cleanup, but intentionally retains local `mem_cap` until a post-handoff release policy (or kernel transfer pinning for generic cap transfers) is defined; default remains inline/copyout fallback (gate disabled).

---

## 1) When to use shared reply

Use shared reply when:

- response payload is larger than practical inline/copyout thresholds for that path; and
- a stable backing `MemoryObject` already exists (or is cheap to allocate/fill) for the response body.

Prefer regular inline/copyout reply when response is small and latency/complexity favors the simple path.

---

## 2) Producer (service) requirements

1. Create/fill a `MemoryObject` containing the reply bytes.
2. Reply via userspace helper `ipc_reply_v2_shared(...)` (or equivalent manual v2 block construction).
3. Ensure transferred cap is a `MemoryObject` cap.
4. Ensure metadata `offset/len` is in-bounds for that object region.
5. Choose mutability flags deliberately:
   - `READ_ONLY` for immutable reply buffers;
   - writable only when protocol semantics require it.
6. Keep a fallback path (inline/copyout) for rollback and canary phases.

---

## 3) Receiver requirements

1. Use `ipc_call_v2_expect_shared(...)` or `decode_shared_reply_response(...)`.
2. Validate metadata decode success before use.
3. Map transferred cap explicitly (no implicit mapping occurs).
4. Handle revocation / map failure / stale-cap errors gracefully.
5. Unmap/release capability lifecycle resources promptly when done.

---

## 4) Required tests per adopting service

- Happy-path shared reply (metadata + transfer cap + explicit map/use).
- Non-`MemoryObject` transfer rejection for shared metadata payload.
- Out-of-bounds shared metadata rejection (`offset/len` invalid).
- Revocation-before-map (or map-failure) handled without crash/data corruption.
- Small-reply inline/copyout fallback still works.

---

## 5) Rollback plan

- Keep copyout path available during rollout.
- Feature-gate service migration so shared-reply can be disabled quickly.
- Maintain compatibility tests for both shared and fallback paths.

---

## 6) Current limitations (Stage 1)

- No automatic mapping in kernel/runtime.
- No request-side CALL transfer-cap channel for shared-reply request payloads.
- No zero-copy DMA policy integration yet.
- Generic `MemoryObject` cap-transfer envelopes are pinned until materialized/purged to preserve transfer lifetime.
- VFS pass 2 provisions real `MemoryObject` producer caps for staged shared-read replies when explicitly enabled.
- VFS pass 2 currently retains local producer `mem_cap` intentionally to avoid pre-handoff lifetime invalidation.
- Consumer shared-reply decode/map migration is deferred to pass 3.
