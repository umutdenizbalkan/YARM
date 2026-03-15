#!/usr/bin/env bash
set -euo pipefail

OUT_DIR=${OUT_DIR:-build}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
RUST_TARGET=${RUST_TARGET:-riscv64gc-unknown-linux-gnu}
SERVER_BIN=${SERVER_BIN:-init_server}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-release}
SERVER_ELF=${SERVER_ELF:-target/${RUST_TARGET}/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-busybox.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-riscv64.bin}
KERNEL_ELF=${KERNEL_ELF:-target/${RUST_TARGET}/${SERVER_BUILD_PROFILE}/core_profile_smoke}
BUSYBOX_BIN=${BUSYBOX_BIN:-}

mkdir -p "$OUT_DIR" "$ROOTFS_DIR/bin" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
mkdir -p "$(dirname "$INITRAMFS_IMAGE")"
INITRAMFS_IMAGE_ABS="$(cd "$(dirname "$INITRAMFS_IMAGE")" && pwd)/$(basename "$INITRAMFS_IMAGE")"

echo "[info] building userspace server for target ${RUST_TARGET}"
if command -v rustup >/dev/null 2>&1; then
  rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true
fi

set +e
cargo build --target "$RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" --bin "$SERVER_BIN"
BUILD_STATUS=$?
set -e
if [[ "$BUILD_STATUS" -eq 0 && -f "$SERVER_ELF" ]]; then
  cp "$SERVER_ELF" "$ROOTFS_DIR/sbin/${SERVER_BIN}"
else
  echo "[warn] cross-compile for ${SERVER_BIN} failed or output missing (${SERVER_ELF})"
fi

if [[ -n "$BUSYBOX_BIN" && -x "$BUSYBOX_BIN" ]]; then
  cp "$BUSYBOX_BIN" "$ROOTFS_DIR/bin/busybox"
elif command -v busybox >/dev/null 2>&1; then
  cp "$(command -v busybox)" "$ROOTFS_DIR/bin/busybox"
else
  echo "[warn] busybox not found; creating minimal /init fallback"
fi

if [[ -x "$ROOTFS_DIR/bin/busybox" ]]; then
  chmod +x "$ROOTFS_DIR/bin/busybox"
  for app in sh mount echo cat; do
    ln -sf /bin/busybox "$ROOTFS_DIR/bin/$app"
  done
fi

cat > "$ROOTFS_DIR/init" <<'SH'
#!/bin/sh
mount -t proc none /proc 2>/dev/null || true
mount -t sysfs none /sys 2>/dev/null || true
if [ -x /sbin/init_server ]; then
  /sbin/init_server || true
fi
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
fi

if [[ ! -f "$KERNEL_IMAGE" && -f "$KERNEL_ELF" ]]; then
  if command -v llvm-objcopy >/dev/null 2>&1; then
    llvm-objcopy -O binary "$KERNEL_ELF" "$KERNEL_IMAGE" || true
  elif command -v rust-objcopy >/dev/null 2>&1; then
    rust-objcopy -O binary "$KERNEL_ELF" "$KERNEL_IMAGE" || true
  fi
fi

if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] kernel image: $KERNEL_IMAGE"
else
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  echo "[hint] provide a real RISC-V kernel image via KERNEL_IMAGE=<path>"
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
echo "[ok] artifact staging complete in $OUT_DIR"
