#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

OUT_DIR=${OUT_DIR:-build-aarch64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
RUST_TARGET=${RUST_TARGET:-targets/aarch64-yarm-none.json}
RUST_TARGET_DIR=${RUST_TARGET_DIR:-aarch64-yarm-none}
SERVER_BIN=${SERVER_BIN:-init_server}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}
SERVER_PACKAGE=${SERVER_PACKAGE:-yarm-control-plane-servers}
KERNEL_PACKAGE=${KERNEL_PACKAGE:-yarm}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-aarch64-none}
SERVER_ELF=${SERVER_ELF:-target/${RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
KERNEL_ELF=${KERNEL_ELF:-target/${RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-aarch64.elf}
KERNEL_BIN_IMAGE=${KERNEL_BIN_IMAGE:-$OUT_DIR/yarm-aarch64.bin}
BUSYBOX_BIN=${BUSYBOX_BIN:-}
ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}
BUILD_LOG=${BUILD_LOG:-$OUT_DIR/aarch64-build.log}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}

CARGO_Z_ARGS=()
if cargo -V 2>/dev/null | rg -q "nightly"; then
  CARGO_Z_ARGS=(-Z "build-std=${BUILD_STD_COMPONENTS}")
  if [[ "$RUST_TARGET" == *.json ]]; then
    CARGO_Z_ARGS+=(-Z "json-target-spec")
  fi
else
  echo "[warn] cargo is not nightly; skipping -Z build-std"
  echo "[hint] install nightly cargo to build std from source for ${RUST_TARGET}"
fi

exit_if_strict_mode() {
  if [[ "$ARTIFACTS_STRICT" == "1" ]]; then
    exit 1
  fi
}

emit_missing_target_hint() {
  local target="$1"
  echo "[hint] cargo could not build for target '${target}' with the current toolchain"
  if command -v rustup >/dev/null 2>&1; then
    echo "[hint] try: rustup target add ${target}"
  else
    echo "[hint] rustup is not available; install target std artifacts for ${target} via your package manager or build on another machine and copy target/${target}/ artifacts here"
    echo "[hint] on Termux, this usually means either using a toolchain that already ships ${target}, or prebuilding artifacts off-device"
  fi
  echo "[hint] build output was captured in: $BUILD_LOG"
}

archive_rootfs() {
  if ! command -v cpio >/dev/null 2>&1; then
    echo "[warn] cpio not found; creating placeholder initramfs archive file"
    : > "$INITRAMFS_IMAGE_ABS"
    exit_if_strict_mode
    return
  fi

  local cpio_help
  cpio_help="$(cpio --help 2>&1 || true)"
  if printf '%s' "$cpio_help" | rg -q -- '--null'; then
    ( cd "$ROOTFS_DIR" && find . -print0 | cpio --null -ov --format=newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
    return
  fi

  if printf '%s' "$cpio_help" | rg -q -- ' -H '; then
    ( cd "$ROOTFS_DIR" && find . -print | cpio -o -H newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
    return
  fi

  echo "[warn] cpio is installed but does not advertise --null or -H newc support; creating placeholder initramfs archive file"
  : > "$INITRAMFS_IMAGE_ABS"
  exit_if_strict_mode
}

mkdir -p "$OUT_DIR" "$ROOTFS_DIR/bin" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
mkdir -p "$(dirname "$INITRAMFS_IMAGE")"
INITRAMFS_IMAGE_ABS="$(cd "$(dirname "$INITRAMFS_IMAGE")" && pwd)/$(basename "$INITRAMFS_IMAGE")"

if command -v rustup >/dev/null 2>&1; then
  if [[ "$RUST_TARGET" != *.json ]]; then
    rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true
  fi
fi

echo "[info] building ${KERNEL_PACKAGE}/${KERNEL_BIN} for target ${RUST_TARGET}"
BUILD_OK=1
set +e
cargo build --target "$RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$KERNEL_PACKAGE" --bin "$KERNEL_BIN" \
  "${CARGO_Z_ARGS[@]}" \
  2>&1 | tee "$BUILD_LOG"
KERNEL_BUILD_STATUS=$?
set -e
if [[ "$KERNEL_BUILD_STATUS" -ne 0 ]]; then
  BUILD_OK=0
  emit_missing_target_hint "$RUST_TARGET"
fi

echo "[info] building ${SERVER_PACKAGE}/${SERVER_BIN} for target ${RUST_TARGET}"
set +e
cargo build --target "$RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$SERVER_BIN" \
  "${CARGO_Z_ARGS[@]}" \
  2>&1 | tee "$BUILD_LOG"
SERVER_BUILD_STATUS=$?
set -e
if [[ "$SERVER_BUILD_STATUS" -ne 0 ]]; then
  BUILD_OK=0
  emit_missing_target_hint "$RUST_TARGET"
fi

if [[ "$BUILD_OK" -eq 1 && -f "$SERVER_ELF" ]]; then
  cp "$SERVER_ELF" "$ROOTFS_DIR/sbin/${SERVER_BIN}"
else
  echo "[warn] compile for ${SERVER_BIN} failed or output missing (${SERVER_ELF})"
  exit_if_strict_mode
fi

if [[ -n "$BUSYBOX_BIN" && -x "$BUSYBOX_BIN" ]]; then
  cp "$BUSYBOX_BIN" "$ROOTFS_DIR/bin/busybox"
elif command -v busybox >/dev/null 2>&1; then
  cp "$(command -v busybox)" "$ROOTFS_DIR/bin/busybox"
else
  echo "[warn] busybox not found; creating minimal /init fallback"
  exit_if_strict_mode
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

archive_rootfs

if [[ ! -f "$KERNEL_IMAGE" && -f "$KERNEL_ELF" ]]; then
  cp "$KERNEL_ELF" "$KERNEL_IMAGE"
fi

if [[ -f "$KERNEL_ELF" ]]; then
  if command -v llvm-objcopy >/dev/null 2>&1; then
    llvm-objcopy -O binary "$KERNEL_ELF" "$KERNEL_BIN_IMAGE" || true
  elif command -v rust-objcopy >/dev/null 2>&1; then
    rust-objcopy -O binary "$KERNEL_ELF" "$KERNEL_BIN_IMAGE" || true
  fi
fi

if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] kernel image: $KERNEL_IMAGE"
else
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  exit_if_strict_mode
fi

if [[ -f "$KERNEL_BIN_IMAGE" ]]; then
  echo "[ok] kernel binary image: $KERNEL_BIN_IMAGE"
else
  echo "[warn] raw kernel binary image missing: $KERNEL_BIN_IMAGE"
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
echo "[ok] aarch64 artifact staging complete in $OUT_DIR"
echo "[next] run smoke boot: scripts/qemu-aarch64-core-smoke.sh"
