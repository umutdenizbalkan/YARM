<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Fixtures staged into every QEMU initramfs

| file | archive path | served as | contents |
|---|---|---|---|
| `vfs-fixture.bin` | `etc/vfs-fixture` | `/initramfs/etc/vfs-fixture` | 300 bytes, `byte[i] = (i * 131 + 43) mod 256`; sha256 `97056678793b338e860d6ebb2ffa74d2f86005ddf2415958836ba0626e00f008` |

`vfs-fixture.bin` is the VFS-CORRECT1A read fixture (`doc/VFS.md`, *Inline read
completions*): longer than one inline read completion (`VFS_INLINE_READ_MAX` = 104 bytes), with
every byte computable from its offset, so a reader can check what it received without trusting
the reply that carried it. Later VFS passes compare against the same bytes. Regenerate with:

```sh
python3 -c "open('scripts/fixtures/vfs-fixture.bin','wb').write(bytes(((i*131+43)&0xFF) for i in range(300)))"
```
