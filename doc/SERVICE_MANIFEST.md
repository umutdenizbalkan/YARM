<!-- SPDX-License-Identifier: Apache-2.0 -->

# Service-list manifest v1

Status: MANIFEST-1 helper-only parser. Nothing in this document or parser is
wired to live init spawning.

## Purpose and ownership

A future boot command line may select a CPIO file with:

```text
yarm.manifest=/boot/services-core.txt
```

The selected file is intended to describe services that **init** may request
after the existing bootstrap sequence. Responsibility remains divided as
follows:

- the kernel captures boot command-line bytes without making service policy;
- init selects and validates the service-list manifest;
- PM remains the spawn authority; and
- the supervisor remains the fault and restart authority.

MANIFEST-1 implements only a bounded userspace parser. It does not receive the
boot command line, read CPIO, call PM, alter the current service list, or change
runtime ordering.

## Parser location and relationship to the executable manifest

The text parser is in:

```text
crates/yarm-fs-servers/src/fs/initramfs/service_manifest.rs
```

The neighboring `manifest.rs` is an existing binary **executable manifest**
parser for core ELF image metadata, entry addresses, and load segments. That
binary format and the text service-list format have different responsibilities
and remain separate.

Audit classification:

- **B:** a new text parser was needed;
- **C:** the initramfs module is the appropriate userspace home;
- **E:** runtime wiring remains deferred; and
- **F:** no parser blocker was found.

The existing CPIO archive API supports a separate existence/ELF validation
stage. MANIFEST-2 implements that stage below without coupling it to v1 syntax
parsing or live policy.

## V1 syntax

The input must be UTF-8 text. Each nonempty, non-comment line contains exactly
one absolute service path.

```text
# QEMU development example
/sbin/initramfs_srv
/sbin/devfs_srv
/sbin/vfs_srv
/sbin/driver_manager
/sbin/blkcache_srv
/sbin/virtio_blk_srv
/sbin/fat_srv
/sbin/ext4_srv
```

Rules:

- LF and CRLF line endings are accepted;
- empty lines and whitespace-only lines are ignored;
- a full-line comment begins with `#`, optionally after leading whitespace;
- inline comments are rejected;
- paths must begin with `/` and may not be `/` alone;
- relative paths are rejected;
- `..` components are rejected;
- `.` components, repeated separators/empty components, and control characters
  are rejected;
- whitespace inside or around a service path is rejected;
- quoted strings and escaping are not supported;
- classes, start policies, metadata fields, and trailing tokens are not
  supported;
- globbing and environment-variable expansion are not supported; and
- duplicate paths are rejected.

The parser is **fail-whole-file**. One invalid service line rejects the complete
manifest; callers must never execute a successfully parsed prefix.

## Bounds and representation

| Limit | Value |
| --- | ---: |
| Manifest bytes | 8192 |
| Line bytes, excluding LF and optional CR | 256 |
| Service path bytes | 255 |
| Service entries | 64 |

`ServiceManifest` uses a fixed array of 64 entries. Each entry owns a fixed
255-byte path buffer, its path length, and its original one-based source line.
The parser does not allocate. Public accessors return only the populated entry
slice and each entry's populated path bytes.

Errors identify the source line where useful:

- empty or comments-only input;
- oversized manifest;
- invalid UTF-8;
- overlong line;
- too many entries;
- relative path;
- invalid path;
- parent component;
- whitespace;
- duplicate path; and
- unsupported inline comment.

## Accepted and rejected examples

Accepted:

```text
/sbin/initramfs_srv
/sbin/devfs_srv
/sbin/vfs_srv
```

Rejected:

```text
sbin/devfs_srv
/sbin/../evil
/sbin/foo bar
/
/sbin/vfs_srv # inline comments are not v1 syntax
```

An empty file and a file containing only comments are also rejected.

## Development fallback policy

MANIFEST-1 deliberately does not add a fallback constant or helper. The current
init code already owns a live hardcoded sequence, and introducing a second code
list could be mistaken for active policy or drift from that sequence.

For future development-QEMU integration, the intended behavior remains:

- absent `yarm.manifest`: use an init-owned built-in minimal core list;
- missing or invalid selected file: log the reason and use that same list;
- partially invalid file: reject the whole file, then apply profile fallback;
- future hardened profile: permit explicit fail-closed behavior.

The following is documentation-only policy data, not a runtime constant:

```text
/sbin/initramfs_srv
/sbin/devfs_srv
/sbin/vfs_srv
/sbin/driver_manager
/sbin/blkcache_srv
/sbin/virtio_blk_srv
```

## Deferred work

### BOOTCMD-3

Design an immutable, compatible handoff of raw command-line bytes or the
selected manifest path to init. Do not overload capability fields, SpawnV5, or
existing startup slots.

### MANIFEST-2 (implemented)

Helper-only validation now checks that every parsed path exists in a supplied
CPIO archive and has regular-file ELF-ident metadata. It remains separate from
VFS and spawning; details follow below.

### Later versioned extensions

A future version may define, with explicit compatibility rules:

- service class fields;
- start policy;
- architecture or platform filters;
- device compatible strings;
- driver dependencies;
- capability requirements; and
- hardened fail-closed profiles.

These fields are intentionally absent from v1 so parsing does not silently
create runtime policy.

## Frozen boundaries

MANIFEST-1 does not change:

- kernel or architecture code;
- syscall ABI or `SYSCALL_COUNT`;
- SpawnV5 ABI or implementation;
- PM loading or spawn semantics;
- VFS or filesystem parser behavior;
- Phase2B or Phase3B logic;
- runtime service order or policy;
- startup slot meanings or count;
- IPC, VM, capability, scheduler, trap, or timer internals;
- driver-manager behavior;
- boot command-line capture; or
- CPIO packer alignment behavior.

## MANIFEST-2 archive and ELF validation

MANIFEST-2 adds the helper-only API:

```text
validate_service_manifest_archive(&ServiceManifest, cpio_bytes)
```

The helper preflights the complete `newc` archive and then validates every
manifest entry. For each service path it requires:

- an archive entry with the same path;
- a regular-file mode;
- at least 16 bytes, the ELF identification size; and
- the leading magic bytes `0x7f`, `E`, `L`, `F`.

`CpioArchive::find` already accepts absolute paths by removing one leading `/`,
so `/sbin/foo` resolves to the CPIO entry `sbin/foo`, and `/init` resolves to
`init`. No additional path normalization is performed by MANIFEST-2 because the
MANIFEST-1 syntax parser has already rejected relative paths, parent components,
empty components, whitespace, and control characters.

The validator is read-only. It does not parse ELF headers or program headers,
check target architecture, call VFS, call PM, spawn a service, or change init
policy. Errors distinguish:

- malformed archive;
- archive lookup failure;
- missing path;
- non-regular entry;
- file shorter than ELF ident; and
- non-ELF magic.

Entry-specific errors retain the fixed `ServiceManifestEntry`, including the
path and original manifest line number.

### CPIO truncation handling

The shared `newc` iterator now reports `CpioError::Truncated` when fewer than 110
header bytes remain before a trailer or when the archive ends without a
`TRAILER!!!` entry. This is a read-only parser correction required to keep a
malformed archive distinct from a valid archive with a missing service path.

### Alignment is not validated here

MANIFEST-2 does not replace or duplicate build-time `ALIGN_PROOF`. The current
CPIO entry API does not expose a file-data offset, and runtime ELF existence
validation should not become a second packer policy. Every packed ELF must still
receive 4096-byte data alignment from the CPIO packer and emit its mandatory
alignment proof.

### Future live flow remains deferred

The intended staged flow is:

1. BOOTCMD-3 gives init an immutable raw command line or manifest path.
2. Init reads the selected manifest text from CPIO.
3. `parse_service_manifest` validates the complete v1 syntax.
4. `validate_service_manifest_archive` verifies that all selected files exist
   and have regular-file ELF-ident metadata.
5. Init applies development fallback or hardened fail-closed policy.
6. PM performs spawning, and the supervisor retains restart authority.

None of these steps is live-wired by MANIFEST-2.

## Raspberry Pi 5 documentation profile

A strict v1-compatible, non-runtime example is available at
`profiles/rpi5/services-core.manifest`, with scope and deferred Pi hardware
services documented in `profiles/rpi5/README.md`. The example uses the current
packed name `/sbin/vfs_server` and deliberately omits service paths that are not
yet guaranteed by common CPIO staging.

The profile does not claim Raspberry Pi 5 boot support and is not consumed by
MANIFEST-1, MANIFEST-2, init, PM, or any image-building script.
