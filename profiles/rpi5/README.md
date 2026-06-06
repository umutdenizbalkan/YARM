<!-- SPDX-License-Identifier: Apache-2.0 -->

# Raspberry Pi 5 service-manifest example

This directory is a **documentation/example-only** Raspberry Pi 5 profile. It
does not enable Raspberry Pi 5 hardware boot, is not packed automatically, and
is not consumed by init or any other runtime component.

## Strict example manifest

`services-core.manifest` follows the MANIFEST-1 v1 syntax:

- UTF-8 text;
- one absolute service path per nonempty line;
- blank lines and full-line `#` comments only;
- no inline comments or metadata fields; and
- no duplicate, relative, or whitespace-containing paths.

The example intentionally contains only paths guaranteed by the current common
QEMU initramfs staging flow and useful as a platform-neutral foundation:

| Path | Intended future role |
| --- | --- |
| `/sbin/initramfs_srv` | Read-only access to files packed in the boot CPIO. |
| `/sbin/devfs_srv` | Device namespace service. |
| `/sbin/vfs_server` | VFS routing and mount coordination. The current packed name is `vfs_server`, not `vfs_srv`. |
| `/sbin/driver_manager` | Future platform-device and driver registration coordination. |
| `/sbin/blkcache_srv` | Platform-neutral block-cache layer above a future Pi block backend. |

The file is not a claim about startup order. It is only prospective selection
data for an init-owned policy that does not exist yet.

## Deferred Raspberry Pi 5 services and drivers

A useful bare-metal Raspberry Pi 5 profile will eventually need more than the
strict example. These items are intentionally documented here rather than
listed in `services-core.manifest`, because their Raspberry Pi 5 implementation,
CPIO staging, or platform integration is not yet guaranteed:

- serial/UART console driver;
- mailbox/property interface driver;
- GPIO driver;
- SD/eMMC/MMC block driver;
- IRQ routing through `irqmux_srv`;
- `ramfs_srv` for writable volatile storage;
- `fat_srv` and `ext4_srv` after a Pi block backend is available;
- driver-manager platform registry and device-tree matching;
- xHCI/USB host support later; and
- optional network stack services such as net-device support, `netmgr_srv`,
  `tcpip_srv`, and `socket_srv` later.

The repository declares several of these service binaries today, but the
current common CPIO packer does not guarantee all of their paths. Add them to a
strict profile only after the relevant artifact build stages and Pi-specific
hardware contracts exist.

## Future boot flow

The intended future command line is:

```text
yarm.manifest=/boot/services-core.txt
```

A future Pi image builder could place this example at that CPIO path, but no
current code performs that selection. The required staged flow is:

1. **BOOTCMD-3** provides an immutable command-line or manifest-path handoff to
   init.
2. Init reads the selected text file from CPIO.
3. The MANIFEST-1 helper validates v1 syntax.
4. The MANIFEST-2 helper checks that every listed path exists as a regular ELF
   file in the archive.
5. Init applies profile fallback or fail-closed policy.
6. PM remains the spawn authority.
7. The supervisor remains the fault and restart authority.

MANIFEST-1 and MANIFEST-2 remain helper-only today. Init does not consume this
file, and the current runtime service order is unchanged.

## CPIO alignment requirement

Every ELF packed into a future Pi CPIO must retain 4096-byte file-data alignment
and produce the mandatory build-time `ALIGN_PROOF ... aligned=true` marker. The
MANIFEST-2 existence/ELF-magic validator does not replace or duplicate this
packer requirement.

## Scope warning

This profile does not assert that:

- Raspberry Pi 5 boots YARM today;
- the listed services have Pi-specific hardware support;
- device-tree discovery or platform matching is implemented;
- the manifest is handed to init; or
- any listed service will be spawned because this file exists.
