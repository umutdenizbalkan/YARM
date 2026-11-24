### 3.1 Request/reply transaction and completions (VFS-CORRECT1A)

One VFS operation is one NR 6 (`IpcCall`) and, **only when NR 6 succeeded**, one blocking
`ipc_recv_v2` on the caller's reply endpoint. Any NR 6 failure — `WouldBlock` included — has
delivered nothing (the kernel's send path returns the error and removes the transfer envelope), so
it is returned at once and no receive is attempted. This holds for the client wrappers
(`yarm_user_rt::vfs_client`), for the router's forward to a backend, and for PM's VFS calls.
Nothing is retried; in particular a READ is never re-sent, because a second READ after an
ambiguous completion could advance the file cursor twice.

NR 7 (`IpcReply`) delivers the reply's payload bytes and at most one transferred capability; the
kernel rebuilds the message from the payload, so the replier's `Message.opcode` does not reach the
caller. Every reply to an operation the router forwards (STATX, OPENAT, READ, CLOSE,
FILE_GRANT_RO), and every router-local error for one, is therefore a **completion carried in the
payload** (`yarm_ipc_abi::vfs_abi::VfsCompletion`):

| offset | size | field | |
|---|---|---|---|
| 0 | 2 | magic | `VFS_COMPLETION_MAGIC` (`b"VC"`) |
| 2 | 2 | op | the `VFS_OP_*` this completes |
| 4 | 4 | status | `VFS_STATUS_OK` or a `VFS_STATUS_ERR_*` |
| 8 | 8 | value | OPENAT: fd; READ: byte count; STATX: the statx value; CLOSE: 0; FILE_GRANT_RO: file length; 0 on error |
| 16 | 8 | fd | the request's fd (READ, CLOSE, FILE_GRANT_RO); 0 for path operations |
| 24 | n | data | READ success only: exactly `value` bytes |

The reply carries a transferred capability exactly when it is a successful FILE_GRANT_RO, and never
a reply capability. All fields are little-endian. Kernel syscall ABI v13 is unchanged; this is a
service wire format.

**Limits.** An NR 7 reply carries at most `Message::MAX_PAYLOAD` = 128 payload bytes, so one READ
completion carries at most `VFS_INLINE_READ_MAX` = 128 − 24 = **104** bytes. NR 6 places the
2-byte opcode in front of the request payload inside the same 128-byte limit, so a request payload
is at most `VFS_REQUEST_PAYLOAD_MAX` = 126 bytes (a READ request is 32).

**Validation** (`validate_vfs_completion`, shared by the client, the router and PM) happens before
a byte is copied, an fd is published or a grant is consumed: magic and length, operation, fd echo,
status (an error carries value 0, no data and no capability), the READ count against the request
(≤ requested, ≤ 104) and against the bytes actually carried (equal), and the capability shape. A
caller can tell apart: a transport failure (`VfsClientError::Send` / `Recv` / `NoReply`), a
malformed completion (`Malformed(fault)`), a router or backend error (`Status(code)`), and end of
file (`Ok(0)` for a nonempty request).

**READ semantics.** `vfs_read(fd, buf)` asks for `min(buf.len(), 104)` bytes. The resident server
asks its backend for at most that many **before** the backend advances its cursor, so the next READ
starts at the first undelivered byte, and the completion carries exactly the bytes delivered; a
backend that reports a count without bytes (no byte source) answers `VFS_STATUS_ERR_UNSUPPORTED`,
never a success. On success the client copies exactly the validated count into `buf[..n]` and
leaves `buf[n..]` untouched; a short read is valid. On any error `buf` is untouched. A zero-length
request is valid and returns 0; EOF repeats.

**Status codes** (`VFS_STATUS_ERR_*`): 1 unknown op, 2 invalid path (including a path the backend
does not have), 3 no mount, 4 bad fd (not in the router's table for this client, or unknown to the
backend), 5 backend transport (forward send or backend receive failed), 6 codec, 7 unsupported, 8
permission (backend path policy), 9 no free fd, 10 backend completion malformed (the router
validated the backend's reply and did not relay it).

**Router.** The router validates each backend completion for the forwarded (op, fd, length) before
it touches its fd table or relays anything: an OPENAT fd is tracked only from a valid success, a
CLOSE removes the entry only on a valid success, and a malformed backend completion is answered with
status 10 instead of being relayed. `VFS_OP_MOUNT_REGISTER` keeps its own 4-byte status reply;
`VFS_OP_READ_BULK` (retired since Stage 197A) is relayed unvalidated.

**PM.** PM publishes an OPENAT fd and consumes a FILE_GRANT_RO MemoryObject only from a validated
completion (status, op, fd echo, a transferred cap present exactly on success); any other outcome
is a hard failure for that image, as before (no fallback exists).

**Fixture.** Every QEMU initramfs carries `etc/vfs-fixture`, served as `/initramfs/etc/vfs-fixture`:
300 bytes, `byte[i] = (i * 131 + 43) mod 256` (`scripts/fixtures/README.md`). Later VFS passes
compare against it.

**Not in this contract** (later passes): full-width capability migration, mount authority, fd
namespaces, grant reclamation, shared buffers and rings, a per-request nonce (association is the
operation plus the fd; the one-shot reply capability and the client's sequential calls cover the
rest), and any other service's send/reply path.
