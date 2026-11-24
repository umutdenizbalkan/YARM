
#### VFS-CORRECT1A (from qualified `1c2b8dfc`)

The first VFS correctness pass, on `wip/vfs-correct1a`. A normal userspace client now opens a
known initramfs file through the resident VFS, reads its exact bytes across inline-reply
boundaries, sees end of file and closes it, and PM still launches a late service through
`VFS_OP_FILE_GRANT_RO` and SpawnFromMemoryObject (NR 29). The canonical contract is
`doc/VFS.md` §3.1. Kernel syscall ABI v13 is unchanged; the change is a service wire format
(below). Scope: audit finding F05 (a resident read reports a count without delivering bytes) and
finding F06's send/reply contract for the resident read path only.

**The baseline defect** (`1c2b8dfc`, `evidence/baseline/`):
- the client wrappers discarded the NR 6 result and received anyway
  (`let _ = ipc_call(...)`, then `ipc_recv_v2`); the router and PM treated an NR 6 `WouldBlock`,
  which delivers nothing, as "blocked normally" and received;
- `vfs_read` returned the count the server reported and never copied a byte. The resident
  handler asked its backend for the client's whole length (the cursor advanced by all of it),
  and a reply too long for one message, or a backend without bytes, became `ReadLen(count)`;
- errors were carried only in the reply's opcode, which NR 7 does not deliver (the kernel
  rebuilds the message from the payload), so a failure arrived as an empty or 4-byte payload;
- PM's grant decoder checked that opcode and ignored the reply's status.

Captured live on AArch64 SMP2 (baseline plus `vc1a-baseline-probe.patch`): init read 64 bytes of
`/initramfs/init` into a sentinel buffer; `count=64`, all 64 bytes still `0xA5`, while the backend's
reply was 80 bytes long. The file starts `7f454c46…`.

**The repair** (`55447e80`):
- **Transaction.** One NR 6 and, only if it succeeded, one receive. Any NR 6 error returns at
  once with no receive, in the client (`VfsClientError::Send`), in the router's forward (an
  error completion, status 5) and in PM (`InvalidTransport`). Nothing is retried; a READ is never
  re-sent.
- **Completion.** Every reply to a forwarded operation (STATX, OPENAT, READ, CLOSE,
  FILE_GRANT_RO), and every router error for one, is a 24-byte `VfsCompletion` in the payload
  (magic `VC`, op, status, value, fd), followed by the READ bytes only.
- **Validation** (`validate_vfs_completion`, one function shared by the client, the router and
  PM) runs before a byte is copied, an fd is tracked or published, or a grant is consumed: magic,
  length, op, fd echo, an error carrying nothing, the READ count against the request and against
  the bytes carried, and the capability shape. Transport failure, malformed completion, status and
  end of file are distinct results.
- **Inline limit.** 128 − 24 = 104 bytes per READ completion; a request payload is at most
  126 bytes (NR 6's 2-byte opcode prefix). The client asks for `min(len, 104)`; the resident
  handler asks the backend for at most that **before** the cursor moves; a backend with no bytes
  answers UNSUPPORTED. The client copies exactly the validated count and leaves the rest of the
  buffer untouched; on any error the buffer is untouched.
- **Router.** It validates each backend completion before touching its fd table: OPENAT is
  tracked only on success, CLOSE removes only on success, a malformed backend completion becomes
  status 10 and is not relayed.
- **PM.** It publishes an OPENAT fd and consumes a FILE_GRANT_RO MemoryObject only from a
  validated completion; everything else fails that image, as before.
- **Fixture.** Every QEMU initramfs carries `etc/vfs-fixture` (300 bytes,
  `byte[i] = (i * 131 + 43) mod 256`, `scripts/fixtures/`).

**Service wire-format change.** The VFS reply format for STATX, OPENAT, READ, CLOSE and
FILE_GRANT_RO, between the router and its clients and between the backends and the router, is now
the completion above (previously an 8-byte little-endian value, a `ReadLen`/inline READ reply, or
an opcode-only error). New statuses 7 to 10 (unsupported, permission, no free fd, backend
completion malformed). Producers (five resident backends, the router), the relay (the router) and
consumers (`yarm_user_rt::vfs_client`, PM) moved together. `VFS_OP_MOUNT_REGISTER` keeps its
4-byte status reply; `VFS_OP_READ_BULK` (retired) is relayed unvalidated; FAT's WRITE_INLINE keeps
its reply. The kernel images are byte-identical to the baseline's on all three ports.

**The witness** (`vfs-correct-witness`, init only; inert elsewhere):
`control_plane/init/vfs_correct_witness.rs` runs after the VFS open smoke, through the production
client wrappers, the resident router and `initramfs_srv`: reads of 13, 200, 1, 104, 64 and 512 bytes
into sentinel-filled buffers (expected 13, 104, 1, 104, 64, 14: two over-long requests and a short
final read), each byte compared with the formula and each tail checked; end of file at 50, 0 and
32 bytes; close; read after close and fd 9999 (status 4), a missing path (2), a path with no mount
(3), the buffers untouched; a fresh open, read and close; and the baseline capture repeated
(64 bytes of `/initramfs/init`). `scripts/grade-vfs-correct-witness.py` (rules CORE, READS, BYTES,
EOF, CLOSE, ERRORS, INIT, GRANT, TRANSPORT) compares the logged bytes with the committed fixture
and with the build's `rootfs/init`, and checks PM's image-7 grant, its NR 29 spawn and the
spawned service's entry. `scripts/qemu-vfs-correct-witness-smoke.sh ARCH SMP`.

**Found on the way.**
- **Run 1 (qual-1-FAILED, candidate `55447e80`).** Two holes in the evidence, none in the repair:
  - the hosted control that removes the resident handler's 104-byte cap (ncC) passed every test:
    the client never asks for more than 104 bytes, so no test reached the server's cap. A raw
    4096-byte READ is now sent to `handle_completion` directly; the control now fails it with
    `Ok(Status(7))`;
  - GRANT required driver_manager's entry line after PM's `PM_SPAWN_FROM_MO_DONE`, which PM logs
    only after NR 29 returns. On the RISC-V boot the kernel ran the child first (entry at line
    11447, DONE at 11564), so a correct boot failed; the live AArch64 control showed the same
    order. The anchor is now the grant PM received before its NR 29 call.
  Both fixes are `5bd96968`. Run 1's other steps passed, and its witness lines were all correct on
  all three ports.
- **Dev incident.** A dev control's restore (`git checkout --` on the client) discarded the then
  uncommitted repair to the client. It was re-applied verbatim from the session record and
  re-tested before commit (`evidence/dev/README.md`). The qualification runner mutates only a
  committed candidate.

**Commits.**
- `55447e80`: the repair, the fixture, the witness, the grader and the smoke script, the contract
  in `doc/VFS.md` §3.1.
- `5bd96968`: the direct cap test and the GRANT anchor (run 1's findings).
- The documentation commit follows `5bd96968`. It is record-only; the tested code commit is
  `5bd96968`.

**Qualification.**
- **Frozen inputs.** The runner, `nc.py`, the grader, the smoke script and the fixture were hashed
  before each run (`predeclared.sha`). Every witness artifact was bound at boot to the
  candidate's clean tree (`source=clean:9dd02db2…`).
- **Run 1** on `55447e80`: FAILED (above). Kept as `qual-1-FAILED`.
- **Run 2** on `5bd96968` (tree `9dd02db2`) is the run below. Every step ran in order; nothing
  was rerun in place.

| step | result |
|---|---|
| hosted | fmt; serial lib, 6,776 passed (the control-plane crate disables its lib and bin tests, so its PM and router changes are covered through the shared validator, the guards in `yarm-fs-servers` and the live boots); `--tests`, 43 binaries, 7,173 passed; crates `yarm-ipc-abi` 221, `yarm-user-rt` 149, `yarm-fs-servers` 579, `yarm-srv-common` 27, `yarm-kernel` 10; focused subsets, each non-zero (`yarm-ipc-abi` `vc1a` 10, `yarm-user-rt` `vfs_client` 29, `yarm-fs-servers` `vc1a` 6 and `stage92` 10, root `syscall_abi` 2 and `grant_ro` 3); grader self-test, 19 mutants |
| hosted decoder coverage | through the production `validate_vfs_completion` and client: failed send (zero receives), truncated payload, bad magic, wrong op, wrong fd, count above the request, count unequal to the bytes carried, data on an error, an unexpected or missing capability, a reply capability; through the production `handle_completion`: exact reads across completion boundaries, an oversized raw READ capped at 104 with the cursor at 104, EOF, close, errors, a backend without bytes |
| gates | contract gate, doc enforcement, the proc/VFS codec freeze |
| checks | default kernel, default init and witness init on all three targets |
| builds | default x86_64, AArch64 and RISC-V artifacts, C and B. C's kernels are byte-identical to B's (`ea10316e…`, `44170087…`, `d539cf7a…`). The core initramfs differs, as it must: the servers, the client, PM and the fixture changed |
| core smokes | the default C kernels, SMP2, GP, each port: pass |
| W x86_64 SMP2 | `VFS_CORRECT_WITNESS_SEAL … result=ok bytes=300 grants=5 late_service_tid=10003 nr29_spawns=5` |
| W AArch64 SMP2 | the same, `result=ok` |
| W RISC-V SMP2 | the same, `result=ok` |
| negative controls | hosted: ncA (the right count, no copy) fails 2 `vfs_client` tests on the destination bytes; ncB (receive after a failed NR 6) fails `vc1a_send_failure_makes_zero_receive_attempts`; ncC (the resident handler's cap removed) fails `vc1a_resident_handler_caps_an_oversized_read_before_the_backend`. Live AArch64, freshly built: ncA, `result=fail`, BYTES "wrong destination bytes: differ from the fixture at offset 0" with READS passing (every count right), plus ERRORS and INIT on their byte checks and CORE (`VC1A_END fails=7`); no hang. Tree restored, porcelain 0 |
| not run | a `--workspace` run; the historical matrix; RT profiles (no scheduler change) |

Per port, every read in the witness:

| read | size | count | offset | bytes | tail |
|---|---|---|---|---|---|
| 0 | 13 | 13 | 0 | exact | untouched |
| 1 | 200 | 104 | 13 | exact | untouched |
| 2 | 1 | 1 | 117 | exact | untouched |
| 3 | 104 | 104 | 118 | exact | untouched |
| 4 | 64 | 64 | 222 | exact | untouched |
| 5 | 512 | 14 | 286 | exact | untouched |
| EOF | 50, 0, 32 | 0, 0, 0 | 300 | — | untouched |

Identical on x86_64, AArch64 and RISC-V. Errors: read after close and fd 9999, status 4; a missing
path, 2; no mount, 3; each destination untouched, then a fresh open, read and close succeeded.
`/initramfs/init`'s first 64 bytes matched each build's `rootfs/init`. Each port: PM received the
image-7 grant, spawned driver_manager through NR 29 (tid 10003), and it ran; five NR 29 spawns
in all.

**Verdict: QUALIFIED** on `5bd96968`.

**Limits carried forward.**
- **Association** is the operation plus the fd; there is no per-request nonce. The one-shot reply
  capability and the client's sequential calls cover the rest.
- **One completion per READ**: at most 104 bytes per call; bulk transfer, shared buffers and rings
  are later passes.
- **Out of scope, unchanged:** init's own PM spawn request still discards its NR 6 result
  (`init/service.rs`, the spawn_v5 call); other services' send/reply paths; capability
  migration, mount authority, fd namespaces and grant reclamation.
- `VFS_OP_READ_BULK` is relayed unvalidated (retired since Stage 197A).

**Audit findings.** F05 closed. F06 partially closed: the send/reply contract holds for the
resident VFS read path (client, router forward, backends, PM's VFS calls); other services are not
covered.
