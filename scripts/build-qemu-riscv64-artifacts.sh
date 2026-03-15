#!/usr/bin/env bash
set -euo pipefail

OUT_DIR=${OUT_DIR:-build}
mkdir -p "$OUT_DIR/rootfs/bin"

# Build a host-side placeholder artifact for early CI integration.
# This is not yet a bootable kernel image; it is a staged bridge until
# arch-specific kernel image generation lands.
cargo build -q --bin init_server
cp target/debug/init_server "$OUT_DIR/yarm-riscv64.bin"

cat > "$OUT_DIR/rootfs/init" <<'SH'
#!/bin/sh
echo "BusyBox placeholder shell"
echo "/ # "
SH
chmod +x "$OUT_DIR/rootfs/init"

# cpio archive for smoke wiring.
(
  cd "$OUT_DIR/rootfs"
  find . -print0 | cpio --null -ov --format=newc > "../initramfs-busybox.cpio"
) >/dev/null 2>&1 || true

echo "[ok] staged artifacts in $OUT_DIR"
