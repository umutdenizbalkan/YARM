# Blkcache IPC ABI (Stage 1)

`blkcache_srv` is storage middleware and is owned by `crates/yarm-driver-servers`.
`driver_manager` does not route normal filesystem IO.

Future direction is:
filesystem -> blkcache -> block-driver service.

IPC carries control metadata only. Block data must not be carried in IPC payloads;
shared buffers / zero-copy transport are future work.

Current stage behavior:
- decode known blkcache opcodes
- validate fixed-size payloads
- reply with `BlkCacheResponse`
- return unsupported for real operations
- return bad-request for malformed payloads

See `crates/yarm-ipc-abi/src/blkcache_abi.rs` for the frozen opcode/status and
wire struct definitions.
