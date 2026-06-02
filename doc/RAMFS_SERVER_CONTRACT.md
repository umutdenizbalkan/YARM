// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

# RAMFS server contract

`ramfs_srv` is a userspace, memory-only filesystem server. It keeps all data in
its own address space and does not persist contents across process restart or
reboot. It is intended as a writable scratch filesystem for early userspace and
hosted-dev tests.

## Mount prefix and startup config

The default mount prefix is `/ram`. Init may override it through the existing
SpawnV5 startup words used by filesystem services:

- slot 14 (`service_extra_cap_1`) stores the packed prefix bytes, up to 8 bytes;
- slot 15 (`initrd_ptr` raw startup word) stores RAMFS metadata;
- metadata bits `0..31` store `max_bytes`;
- metadata bits `32..47` store flags, where bit 0 is the readonly flag;
- metadata bits `48..55` store the prefix length;
- metadata bits `56..63` store the userspace-only RAMFS config source tag.

No kernel ABI or SpawnV5 semantic change is required. If config is missing,
`ramfs_srv` logs `RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config` and
uses the writable `/ram` compatibility default. When config is present it logs
`RAMFS_CONFIG_FOUND prefix=...`.

## Supported VFS behavior

The RAMFS core supports directories, regular files, normalized absolute paths,
repeated slash handling, file creation through the core API, byte reads/writes,
mkdir, unlink of regular files, stat metadata, and capacity checks. The current
VFS request ABI exposes `openat`, `read`, `write`, `close`, and `statx`; those are
wired through the common `FsService` model. The core mkdir/unlink APIs are kept
ready for a future VFS ABI opcode, but no mkdir/unlink VFS opcode exists today.
Unsupported VFS operations return `VfsError::Unsupported` and log
`RAMFS_UNSUPPORTED_OP op=...` where the backend sees the operation.

`VFS_OP_WRITE` currently carries a buffer pointer and length through the common
server interface but does not provide a safe cross-address-space byte copy helper
to RAMFS. Therefore the VFS write path extends the file with zero bytes and
tracks the written length, while hosted/core tests exercise exact byte writes via
`write_bytes`.

## Init/PM/VFS wiring

Init can spawn `ramfs_srv` as image id 11, pass the RAMFS startup config in
startup slots 14/15, and register the selected prefix with VFS using the existing
`VFS_OP_MOUNT_REGISTER` request. Process manager resolves image id 11 to
`/initramfs/sbin/ramfs_srv` and logs `PM_IMAGE_ID_11_RAMFS_SRV path=...`.

Smoke-visible markers for the RAMFS path are:

- `INIT_RAMFS_SPAWN_BEGIN`
- `INIT_RAMFS_SPAWN_OK`
- `PM_IMAGE_ID_11_RAMFS_SRV path=...`
- `RAMFS_BIN_ENTRY_START`
- `RAMFS_BIN_BEFORE_RUN`
- `RAMFS_SRV_ENTRY`
- `RAMFS_CONFIG_FOUND prefix=...`
- `RAMFS_CONFIG_DEFAULT prefix=/ram reason=missing-config`
- `RAMFS_MOUNT_READY prefix=...`
- `RAMFS_MOUNT_FAILED reason=...`
- `VFS_MOUNT_REGISTER_RAMFS_OK prefix=...`

The QEMU core smoke scripts count RAMFS markers only when
`RAMFS_SMOKE_EXPECTED=1` is set, so default profiles without RAMFS are not forced
to fail. When RAMFS is expected, the smoke scripts require the spawn markers,
`PM_IMAGE_ID_11_RAMFS_SRV`, `RAMFS_MOUNT_READY`, `VFS_MOUNT_REGISTER_RAMFS_OK`,
and at least one RAMFS config marker (`RAMFS_CONFIG_FOUND` or
`RAMFS_CONFIG_DEFAULT`).

## Limits and errors

The default RAMFS capacity is 512 KiB and 128 nodes. Custom startup config can
lower or raise the byte limit; node count remains a server-side constant for now.
Capacity failures map to existing VFS capacity-style errors instead of panicking.
Bad paths, missing entries, directory/file misuse, and unsupported operations all
return explicit errors through the existing VFS error vocabulary.
