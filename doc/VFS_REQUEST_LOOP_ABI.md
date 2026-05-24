<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# VFS Request-Loop ABI

This document defines the authoritative contract for the VFS server's two-phase
startup, capability requirements, mount table, fd table, opcode encoding, routing
rules, and reply status codes as implemented in
`crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs`.

---

## Position in the Capability Graph

The VFS server requires three capabilities to be present in its startup slots
before it can enter its resident loop:

| Slot | Constant                          | Content                                        |
|------|-----------------------------------|------------------------------------------------|
| 12   | `process_manager_service_recv_ep` | VFS server's own IPC receive endpoint cap      |
| 13   | `service_extra_cap_0`             | send cap to `initramfs_srv` (for CPIO lookups) |
| 14   | `service_extra_cap_1`             | send cap to `devfs_srv` (for `/dev/` requests) |

These caps are delivered by the PM spawn path: the init server places the
initramfs and devfs send caps into `SpawnV5CapArgs.service_caps[0]` and
`[1]` before calling `PROC_OP_SPAWN_V5_CAP` for image_id=6. PM copies them to
slots 13 and 14 of the VFS task's startup args.

Slot 12 is the recv endpoint cap the VFS server blocks on in its resident loop.
It is distinct from PM's recv cap.

---

## Phase 1: Setup Loop (`run_request_loop`)

After reading its startup context, the VFS server enters a setup phase via
`run_request_loop`. During this phase it accepts control-plane probe messages on
the path prefix `b"/control-plane/vfs-probe"`.

Purpose: allow the PM or init server to verify the VFS server is live and has
completed mount-table registration before any client opens files. The setup loop
uses `synthetic_roundtrip_call_reply_with_budget` (from
`crates/yarm-control-plane-servers/src/control_plane/ipc_roundtrip.rs`) which
internally calls `ipc_recv_with_deadline` (timed receive) and `ipc_reply`
(reply-cap send). This satisfies the phase-6 migration invariant: no legacy
blocking `kernel.ipc_recv(` calls.

The setup loop exits when the probe is acknowledged and control transfers to
Phase 2.

---

## Phase 2: Resident Loop

The resident loop calls `ipc_recv_v2(recv_cap)` on slot 12 indefinitely.
Each iteration:

1. Receive a `(Message, reply_cap)` pair.
2. Decode the opcode from the message header.
3. Route the message (see Routing Rules below).
4. Send a reply on `reply_cap`.

If slot 12 is absent (cap = 0) at startup, the server emits
`VFS_SRV_NO_RECV_CAP_RESIDENT_YIELD` and enters a yield loop — it never
becomes reachable for client requests.

---

## Mount Table Contract

The mount table maps path prefixes to backend send caps. Registration happens
during Phase 1 startup:

| Prefix          | Backend         | Slot source |
|-----------------|-----------------|-------------|
| `/initramfs/`   | `initramfs_srv` | slot 13     |
| `/dev/`         | `devfs_srv`     | slot 14     |

Rules:
- Prefix matching is longest-prefix first.
- Exact mount-root matching is supported (e.g. `/dev` matches the `/dev/` mount).
- A path that matches no prefix is rejected with `VFS_STATUS_ERR_NO_MOUNT`.
- Dynamic mount registration is supported via `VFS_OP_MOUNT_REGISTER`:
  - prefix is normalized before insertion,
  - duplicate normalized prefixes are rejected,
  - zero send-cap and invalid/oversized prefixes are rejected,
  - successful insertion participates in longest-prefix routing.

---

## FD Table Contract

The fd table maps `(fd, client_id)` pairs to backend send caps for post-open
operations. `client_id` is the reply-cap value delivered alongside each IPC
request; in this microkernel's IPC model that value encodes the calling thread's
identity and remains stable across calls from the same thread.

An fd entry is created by `VFS_OP_OPENAT` on success and removed by
`VFS_OP_CLOSE` from the owning client.

- FD values are assigned by the backend and are unique within a client session.
- The same numeric fd may be held by different clients simultaneously without
  conflict — each is tracked as a separate `(fd, client_id)` entry.
- A `VFS_OP_READ` or `VFS_OP_CLOSE` for an fd not owned by the calling client
  returns `VFS_STATUS_ERR_BAD_FD`, whether the fd is absent entirely or belongs
  to a different client.
- `VFS_OP_CLOSE` only evicts the calling client's own entry; another client's
  entry for the same fd number is left untouched.
- The fd table capacity is bounded by `MAX_FD_ENTRIES = 32`.

---

## Opcode Reference

Source: `crates/yarm-ipc-abi/src/vfs_abi.rs`

| Opcode | Constant        | Value | Direction     | Description                     |
|--------|-----------------|-------|---------------|---------------------------------|
| STATX  | `VFS_OP_STATX`  | 22    | client → VFS  | stat a path (no fd required)    |
| OPENAT | `VFS_OP_OPENAT` | 10    | client → VFS  | open a file, returns fd         |
| READ   | `VFS_OP_READ`   | 12    | client → VFS  | read from open fd               |
| CLOSE  | `VFS_OP_CLOSE`  | 11    | client → VFS  | close fd, release table entry   |

### STATX (`VFS_OP_STATX = 22`)

Request message body layout — total header = 25 bytes, then inline path:

| Offset | Size | Field        | Description                          |
|--------|------|--------------|--------------------------------------|
| 0      | 8    | `dirfd`      | directory fd (AT_FDCWD = -1 as u64)  |
| 8      | 8    | `flags`      | stat flags                           |
| 16     | 8    | `mask`       | statx mask                           |
| 24     | 1    | `path_len`   | byte length of the inline path       |
| 25     | N    | `path`       | UTF-8 path, max `VFS_STATX_INLINE_PATH_MAX = 96` bytes |

`path_len` must be ≤ 96. Paths longer than 96 bytes cannot be expressed in a
single message and are rejected with `VFS_STATUS_ERR_INVALID_PATH`.

### OPENAT (`VFS_OP_OPENAT = 10`)

Request message body layout — total header = 25 bytes, then inline path:

| Offset | Size | Field        | Description                          |
|--------|------|--------------|--------------------------------------|
| 0      | 8    | `dirfd`      | directory fd (AT_FDCWD = -1 as u64)  |
| 8      | 8    | `flags`      | open flags (O_RDONLY etc.)           |
| 16     | 8    | `mode`       | creation mode (0 for read-only)      |
| 24     | 1    | `path_len`   | byte length of the inline path       |
| 25     | N    | `path`       | UTF-8 path, max `VFS_OPENAT_INLINE_PATH_MAX = 96` bytes |

Reply on success: 8-byte LE fd value.

### READ (`VFS_OP_READ = 12`)

Request: 8-byte LE fd, 8-byte LE offset, 8-byte LE length.  
Reply: inline data bytes (up to message payload limit).

### CLOSE (`VFS_OP_CLOSE = 11`)

Request: 8-byte LE fd.  
Reply: 8-byte LE status (0 = ok).

---

## Reply Status Codes

All error replies generated locally by the VFS router (not forwarded from
backends) carry a 4-byte little-endian `u32` status in the reply message payload.
The reply message opcode is `1` for any locally generated error; `0` for a
forwarded backend success reply.

Source: `crates/yarm-ipc-abi/src/vfs_abi.rs`

| Constant                      | Value | Trigger                                                  |
|-------------------------------|-------|----------------------------------------------------------|
| `VFS_STATUS_OK`               | 0     | Success (backend reply forwarded verbatim)               |
| `VFS_STATUS_ERR_UNKNOWN_OP`   | 1     | Opcode not handled by the VFS router                     |
| `VFS_STATUS_ERR_INVALID_PATH` | 2     | Path payload malformed, or normalization rejection       |
| `VFS_STATUS_ERR_NO_MOUNT`     | 3     | Normalized path matches no mount-table prefix            |
| `VFS_STATUS_ERR_BAD_FD`       | 4     | Fd absent from table, or owned by a different client     |
| `VFS_STATUS_ERR_BACKEND`      | 5     | Backend IPC timed out or returned no reply               |
| `VFS_STATUS_ERR_CODEC`        | 6     | READ/CLOSE payload could not be decoded                  |

### Error Path Mapping

| Condition                                          | Status code                      |
|----------------------------------------------------|----------------------------------|
| Opcode not STATX/OPENAT/READ/CLOSE                 | `VFS_STATUS_ERR_UNKNOWN_OP`      |
| STATX/OPENAT path payload malformed (decode fails) | `VFS_STATUS_ERR_INVALID_PATH`    |
| Path empty, non-absolute, or exceeds 96 bytes      | `VFS_STATUS_ERR_INVALID_PATH`    |
| `..` components at root                             | clamped to `/` (not an error)    |
| Normalized path matches no mount prefix            | `VFS_STATUS_ERR_NO_MOUNT`        |
| READ/CLOSE payload decode failure (malformed args) | `VFS_STATUS_ERR_CODEC`           |
| Fd not present in table                            | `VFS_STATUS_ERR_BAD_FD`          |
| Fd present but owned by a different client         | `VFS_STATUS_ERR_BAD_FD`          |
| Backend IPC timed out or no reply                  | `VFS_STATUS_ERR_BACKEND`         |

The helper `encode_vfs_status(status: u32) -> [u8; 4]` and
`decode_vfs_status(payload: &[u8]) -> Option<u32>` in `vfs_abi.rs` provide
the canonical encode/decode for these 4-byte payloads.

---

## Routing Rules

1. **STATX and OPENAT** — path-based routing:
   - Decode the inline path from the message body.  Malformed payload →
     `VFS_STATUS_ERR_INVALID_PATH`.
   - Normalize the path (collapse `//`, resolve `.`, clamp `..` at `/`).
     Normalization failure → `VFS_STATUS_ERR_INVALID_PATH`.
   - Look up the longest matching prefix in the mount table.  No match →
     `VFS_STATUS_ERR_NO_MOUNT`.
   - Forward the message to the corresponding backend send cap.
   - Return the backend's reply verbatim.

2. **READ and CLOSE** — fd-based routing:
   - Decode the fd from the message body.  Malformed payload →
     `VFS_STATUS_ERR_CODEC`.
   - Look up `(fd, client_id)` in the fd table.  Miss or wrong client →
     `VFS_STATUS_ERR_BAD_FD`.
   - Forward to the backend send cap recorded at OPENAT time.
   - On CLOSE, remove the calling client's fd entry after the backend confirms
     success.

3. **Unknown opcode** — reply `VFS_STATUS_ERR_UNKNOWN_OP` immediately; do not
   forward.

4. **Backend timeout** — if `ipc_recv_with_deadline` returns no reply after
   forwarding, reply `VFS_STATUS_ERR_BACKEND` to the client.

---

## No-Recv-Cap Fallback

If `process_manager_service_recv_ep` (slot 12) is zero at startup:

```
VFS_SRV_NO_RECV_CAP_RESIDENT_YIELD  ← logged once
loop { yield_now() }                 ← permanent yield, never receives
```

The VFS server in this state is a no-op stub. It will not crash the system but
is unreachable for any client request. This condition indicates a PM spawn bug
(slot 12 was not populated).

---

## Forbidden Patterns

- **`kernel.ipc_recv(`** in VFS service code: legacy blocking receive; use
  `ipc_recv_v2` or `ipc_recv_with_deadline` via the roundtrip helper.
- **`ipc_send(server_send_cap`** for replies in the call/reply path: ad-hoc
  server-send reply hops are retired. Replies go through the reply cap returned
  by `ipc_recv_v2`.
- **Direct path forwarding bypassing the mount table**: the routing must always
  consult the mount table; hard-coded backend cap selection is forbidden.
- **Serving clients before Phase 1 completes**: the resident loop must not start
  before the setup probe acknowledges VFS readiness.
- **Generic empty error payloads**: locally generated error replies must encode
  the canonical `VFS_STATUS_ERR_*` status as a 4-byte LE u32 payload; empty
  payloads `&[]` are forbidden for error replies.

---

## Log Markers

| Marker                                   | Phase   | Description                                          |
|------------------------------------------|---------|------------------------------------------------------|
| `VFS_SRV_ENTRY`                          | startup | VFS server binary entry                              |
| `VFS_SRV_RECV_CAP cap=N`                | startup | slot 12 cap value read from startup context          |
| `VFS_SRV_INITRAMFS_SEND_CAP cap=N`      | startup | slot 13 cap value (initramfs backend)                |
| `VFS_SRV_DEVFS_SEND_CAP cap=N`          | startup | slot 14 cap value (devfs backend)                    |
| `VFS_SRV_NO_RECV_CAP_RESIDENT_YIELD`    | startup | slot 12 absent; entering yield fallback              |
| `VFS_SRV_MOUNT_REGISTERED prefix=...`   | setup   | mount table entry confirmed                          |
| `VFS_SRV_RESIDENT_LOOP_ENTER`           | loop    | Phase 2 resident loop starting                       |
| `VFS_SRV_RECV_MSG op=N`                | loop    | message received with opcode N                       |
| `VFS_PATH_NORM_REJECT reason=...`       | loop    | path normalization failed; replies ERR_INVALID_PATH  |
| `VFS_ROUTE_LOOKUP path=... target=...`  | loop    | path routed to named backend                         |
| `VFS_ROUTE_NO_MOUNT path=...`           | loop    | path valid but no mount prefix matches               |
| `VFS_ROUTE_FD_LOOKUP fd=N target=...`  | loop    | fd routed to named backend                           |
| `VFS_ROUTE_BAD_FD fd=N`               | loop    | fd not found or belongs to different client          |
| `VFS_ROUTE_CODEC_ERR`                  | loop    | READ/CLOSE payload decode failure                    |
| `VFS_ROUTE_UNKNOWN_OP op=N`            | loop    | opcode not handled by router                         |
| `VFS_ROUTE_ERR status=N`              | loop    | local error reply sent with status code N            |
| `VFS_ROUTE_REPLY status=0`             | loop    | backend reply forwarded to client                    |
| `VFS_ROUTE_REPLY status=1`             | loop    | backend timeout; ERR_BACKEND reply sent              |
