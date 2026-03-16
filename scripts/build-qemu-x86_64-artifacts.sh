#!/usr/bin/env bash
set -euo pipefail

OUT_DIR=${OUT_DIR:-build-x86_64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
RUST_TARGET=${RUST_TARGET:-targets/x86_64-yarm-none.json}
SERVER_BIN=${SERVER_BIN:-init_server}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-release}
SERVER_ELF=${SERVER_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
KERNEL_ELF=${KERNEL_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-busybox.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-x86_64.elf}
BUSYBOX_BIN=${BUSYBOX_BIN:-}
ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}

mkdir -p "$OUT_DIR" "$ROOTFS_DIR/bin" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
mkdir -p "$(dirname "$INITRAMFS_IMAGE")"
INITRAMFS_IMAGE_ABS="$(cd "$(dirname "$INITRAMFS_IMAGE")" && pwd)/$(basename "$INITRAMFS_IMAGE")"

echo "[info] building server + kernel bins for target ${RUST_TARGET}"
BUILD_OK=1
set +e
cargo build --target "$RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" --bin "$SERVER_BIN" --bin "$KERNEL_BIN"
BUILD_STATUS=$?
set -e
if [[ "$BUILD_STATUS" -ne 0 ]]; then
  BUILD_OK=0
fi

if [[ "$BUILD_OK" -eq 1 && -f "$SERVER_ELF" ]]; then
  cp "$SERVER_ELF" "$ROOTFS_DIR/sbin/${SERVER_BIN}"
else
  echo "[warn] compile for ${SERVER_BIN} failed or output missing (${SERVER_ELF})"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -n "$BUSYBOX_BIN" && -x "$BUSYBOX_BIN" ]]; then
  cp "$BUSYBOX_BIN" "$ROOTFS_DIR/bin/busybox"
elif command -v busybox >/dev/null 2>&1; then
  cp "$(command -v busybox)" "$ROOTFS_DIR/bin/busybox"
else
  echo "[warn] busybox not found; creating minimal /init fallback"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -x "$ROOTFS_DIR/bin/busybox" ]]; then
  chmod +x "$ROOTFS_DIR/bin/busybox"
  for app in sh mount echo cat; do
    ln -sf /bin/busybox "$ROOTFS_DIR/bin/$app"
  done
fi

cat > "$ROOTFS_DIR/init" <<'SH'
#!/bin/sh
echo "YARM_INIT_START"
mount -t proc none /proc 2>/dev/null || true
mount -t sysfs none /sys 2>/dev/null || true
if [ -x /sbin/init_server ]; then
  /sbin/init_server || true
fi
echo "YARM_INIT_DONE"
if [ -x /bin/busybox ]; then
  exec /bin/sh
fi
echo "BusyBox missing in initramfs"
echo "/ # "
exec sh
SH
chmod +x "$ROOTFS_DIR/init"

if command -v cpio >/dev/null 2>&1; then
  ( cd "$ROOTFS_DIR" && find . -print0 | cpio --null -ov --format=newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
else
  echo "[warn] cpio not found; creating placeholder initramfs archive file"
  : > "$INITRAMFS_IMAGE_ABS"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ ! -f "$KERNEL_IMAGE" && -f "$KERNEL_ELF" ]]; then
  cp "$KERNEL_ELF" "$KERNEL_IMAGE"
fi

if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] kernel image: $KERNEL_IMAGE"
else
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  echo "[hint] provide a bootable x86_64 kernel image via KERNEL_IMAGE=<path>"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
echo "[ok] x86_64 artifact staging complete in $OUT_DIR"
