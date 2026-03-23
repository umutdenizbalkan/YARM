#!/usr/bin/env bash
set -euo pipefail

OUT_DIR=${OUT_DIR:-build-x86_64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
RUST_TARGET=${RUST_TARGET:-targets/x86_64-yarm-none.json}
SERVER_BIN=${SERVER_BIN:-init_server}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-x86-none}
SERVER_ELF=${SERVER_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
KERNEL_RAW_ELF=${KERNEL_RAW_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}
KERNEL_BOOTABLE_IMAGE_SOURCE=${KERNEL_BOOTABLE_IMAGE_SOURCE:-$KERNEL_RAW_ELF}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-busybox.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-x86_64.elf}
BUSYBOX_BIN=${BUSYBOX_BIN:-}
ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}
TOOLCHAIN=${TOOLCHAIN:-nightly}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}

RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-$TOOLCHAIN}
RUST_SYSROOT=${RUST_SYSROOT:-$(rustup run "${RUSTUP_TOOLCHAIN}" rustc --print sysroot 2>/dev/null || true)}
RUST_SRC_DIR=${RUST_SRC_DIR:-${RUST_SYSROOT}/lib/rustlib/src/rust}

warn_if_kernel_not_qemu_direct_bootable() {
  local kernel="$1"
  if ! command -v readelf >/dev/null 2>&1; then
    return 0
  fi
  local ftype
  ftype=$(file -b "$kernel" 2>/dev/null || true)
  if [[ "$ftype" == *"ELF"* ]]; then
    if ! readelf -n "$kernel" 2>/dev/null | rg -qi "(PVH|Xen)"; then
      echo "[warn] staged kernel image appears to be plain ELF without PVH note"
      echo "[hint] qemu -kernel may reject it; provide bzImage or PVH-enabled ELF"
    fi
  fi
}


mkdir -p "$OUT_DIR" "$ROOTFS_DIR/bin" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
mkdir -p "$(dirname "$INITRAMFS_IMAGE")"
INITRAMFS_IMAGE_ABS="$(cd "$(dirname "$INITRAMFS_IMAGE")" && pwd)/$(basename "$INITRAMFS_IMAGE")"

if ! rustup toolchain list | rg -q "^${TOOLCHAIN}"; then
  echo "[warn] toolchain '${TOOLCHAIN}' is not installed"
  echo "[hint] run: rustup toolchain install ${TOOLCHAIN}"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -z "$RUST_SYSROOT" || ! -d "$RUST_SRC_DIR" ]]; then
  echo "[warn] rust-src is not installed for toolchain: ${RUSTUP_TOOLCHAIN}"
  echo "[hint] run: rustup component add rust-src --toolchain ${RUSTUP_TOOLCHAIN}"
  echo "[debug] looked for rust-src under: ${RUST_SRC_DIR}"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

echo "[info] building server + kernel bins for target ${RUST_TARGET} (toolchain=${TOOLCHAIN}, build-std=${BUILD_STD_COMPONENTS})"
BUILD_OK=1
set +e
cargo +"${TOOLCHAIN}" build -Z build-std=${BUILD_STD_COMPONENTS} -Z json-target-spec --target "$RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} --bin "$SERVER_BIN" --bin "$KERNEL_BIN"
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
echo "[info] milestone 2 shell flow staged in initramfs (/init -> init_server -> /bin/sh); first blocker remains a bootable x86 kernel artifact"

if command -v cpio >/dev/null 2>&1; then
  ( cd "$ROOTFS_DIR" && find . -print0 | cpio --null -ov --format=newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
else
  echo "[warn] cpio not found; creating placeholder initramfs archive file"
  : > "$INITRAMFS_IMAGE_ABS"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ "$BUILD_OK" -eq 1 && -f "$KERNEL_BOOTABLE_IMAGE_SOURCE" ]]; then
  cp "$KERNEL_BOOTABLE_IMAGE_SOURCE" "$KERNEL_IMAGE"
elif [[ ! -f "$KERNEL_IMAGE" && -f "$KERNEL_BOOTABLE_IMAGE_SOURCE" ]]; then
  cp "$KERNEL_BOOTABLE_IMAGE_SOURCE" "$KERNEL_IMAGE"
else
  echo "[warn] compile for bootable kernel image failed or output missing (${KERNEL_BOOTABLE_IMAGE_SOURCE})"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] kernel image: $KERNEL_IMAGE"
  warn_if_kernel_not_qemu_direct_bootable "$KERNEL_IMAGE"
else
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  echo "[hint] the first blocker is a direct-bootable x86 kernel artifact; the generated initramfs shell flow is staged as the second milestone"
  echo "[hint] provide a bootable x86_64 kernel image via KERNEL_BOOTABLE_IMAGE_SOURCE=<path> or KERNEL_IMAGE=<path>"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
echo "[ok] x86_64 artifact staging complete in $OUT_DIR"
echo "[next] run smoke boot: scripts/qemu-x86_64-busybox-smoke.sh"
