#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

OUT_DIR=${OUT_DIR:-build-x86_64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
RUST_TARGET=${RUST_TARGET:-targets/x86_64-yarm-none.json}
SERVER_BIN=${SERVER_BIN:-init_server}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-x86-none}
SERVER_ELF=${SERVER_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
KERNEL_RAW_ELF=${KERNEL_RAW_ELF:-target/x86_64-yarm-none/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}
KERNEL_BOOTABLE_IMAGE_SOURCE=${KERNEL_BOOTABLE_IMAGE_SOURCE:-}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/bootable-kernel.img}
KERNEL_DEBUG_ELF=${KERNEL_DEBUG_ELF:-$OUT_DIR/${KERNEL_BIN}.elf}
ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}
TOOLCHAIN=${TOOLCHAIN:-nightly}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}
QEMU_X86_ALLOW_ELF_KERNEL=${QEMU_X86_ALLOW_ELF_KERNEL:-1}

RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-$TOOLCHAIN}
RUST_SYSROOT=${RUST_SYSROOT:-$(rustup run "${RUSTUP_TOOLCHAIN}" rustc --print sysroot 2>/dev/null || true)}
RUST_SRC_DIR=${RUST_SRC_DIR:-${RUST_SYSROOT}/lib/rustlib/src/rust}

is_qemu_direct_bootable_x86_kernel() {
  local kernel="$1"
  if [[ ! -f "$kernel" ]]; then
    return 1
  fi
  local ftype
  if command -v file >/dev/null 2>&1; then
    ftype=$(file -b "$kernel" 2>/dev/null || true)
  elif command -v readelf >/dev/null 2>&1 && readelf -h "$kernel" >/dev/null 2>&1; then
    ftype="ELF"
  else
    return 1
  fi
  if [[ "$ftype" != *"ELF"* ]]; then
    return 0
  fi
  if [[ "$QEMU_X86_ALLOW_ELF_KERNEL" != "1" ]]; then
    return 1
  fi
  if ! command -v readelf >/dev/null 2>&1; then
    return 1
  fi
  if ! readelf -l "$kernel" 2>/dev/null | rg -q "NOTE"; then
    return 1
  fi
  if readelf -n "$kernel" 2>/dev/null | rg -qi "(PVH|Xen)"; then
    return 0
  fi
  if readelf -S "$kernel" 2>/dev/null | rg -q "\.note\.Xen"; then
    return 0
  fi
  return 1
}

explain_nonbootable_kernel_source() {
  local kernel="$1"
  if [[ ! -f "$kernel" ]]; then
    echo "[warn] kernel boot source missing: $kernel"
    return
  fi
  local ftype
  if command -v file >/dev/null 2>&1; then
    ftype=$(file -b "$kernel" 2>/dev/null || true)
  elif command -v readelf >/dev/null 2>&1 && readelf -h "$kernel" >/dev/null 2>&1; then
    ftype="ELF"
  else
    ftype="unknown"
  fi
  if [[ "$ftype" == *"ELF"* ]]; then
    if [[ "$QEMU_X86_ALLOW_ELF_KERNEL" != "1" ]]; then
      echo "[warn] ELF kernels are rejected by default for qemu x86 direct-boot staging in this script"
      echo "[hint] provide a known bootable non-ELF image via KERNEL_BOOTABLE_IMAGE_SOURCE=<path> (for example Linux bzImage), or set QEMU_X86_ALLOW_ELF_KERNEL=1 to opt-in to PVH ELF probing"
      echo "[hint] helper: scripts/fetch-linux-bzimage.sh"
      return
    fi
    echo "[warn] freestanding ELF kernel is missing a verified PVH note / entry contract for qemu-system-x86_64 direct boot"
    echo "[hint] the built ${KERNEL_BIN} ELF is kept as a debug artifact until it advertises a loadable PVH entrypoint"
    echo "[hint] provide a known bootable x86_64 kernel image via KERNEL_BOOTABLE_IMAGE_SOURCE=<path> (for example a Linux bzImage or a verified PVH-enabled ELF)"
    return
  fi
  echo "[warn] kernel boot source does not look like a verified qemu -kernel artifact: $kernel"
}


mkdir -p "$OUT_DIR" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
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

if [[ "$BUILD_OK" -eq 1 && -f "$KERNEL_RAW_ELF" ]]; then
  cp "$KERNEL_RAW_ELF" "$KERNEL_DEBUG_ELF"
  echo "[info] yarm freestanding kernel ELF (debug-only): $KERNEL_DEBUG_ELF"
  echo "[info] x86_64 bootstrap invariant script removed; skipping legacy invariant check stage"
fi

cat > "$ROOTFS_DIR/init" <<'SH'
#!/bin/sh
echo "YARM_INIT_START"
if [ -x /sbin/init_server ]; then
  /sbin/init_server || true
fi
echo "YARM_INIT_DONE"
while true; do
  :
done
SH
chmod +x "$ROOTFS_DIR/init"
echo "[info] staged minimal initramfs marker flow (/init -> init_server) while primary x86 goal remains kernel serial markers"

if command -v cpio >/dev/null 2>&1; then
  ( cd "$ROOTFS_DIR" && find . -print0 | cpio --null -ov --format=newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
else
  echo "[warn] cpio not found; creating placeholder initramfs archive file"
  : > "$INITRAMFS_IMAGE_ABS"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

BOOTABLE_SOURCE=${KERNEL_BOOTABLE_IMAGE_SOURCE:-}
if [[ -z "$BOOTABLE_SOURCE" && -f "$KERNEL_IMAGE" ]]; then
  BOOTABLE_SOURCE="$KERNEL_IMAGE"
fi

if [[ -n "$BOOTABLE_SOURCE" && -f "$BOOTABLE_SOURCE" ]] && is_qemu_direct_bootable_x86_kernel "$BOOTABLE_SOURCE"; then
  if [[ "$BOOTABLE_SOURCE" != "$KERNEL_IMAGE" ]]; then
    cp "$BOOTABLE_SOURCE" "$KERNEL_IMAGE"
  fi
  echo "[info] bootable kernel source selected: $BOOTABLE_SOURCE"
elif [[ -n "$BOOTABLE_SOURCE" && -f "$BOOTABLE_SOURCE" ]]; then
  rm -f "$KERNEL_IMAGE"
  explain_nonbootable_kernel_source "$BOOTABLE_SOURCE"
elif [[ "$BUILD_OK" -eq 1 && -f "$KERNEL_RAW_ELF" ]] && is_qemu_direct_bootable_x86_kernel "$KERNEL_RAW_ELF"; then
  cp "$KERNEL_RAW_ELF" "$KERNEL_IMAGE"
elif [[ "$BUILD_OK" -eq 1 && -f "$KERNEL_RAW_ELF" ]]; then
  rm -f "$KERNEL_IMAGE"
  explain_nonbootable_kernel_source "$KERNEL_RAW_ELF"
else
  echo "[warn] compile for bootable kernel image failed or output missing (${KERNEL_BOOTABLE_IMAGE_SOURCE:-unset})"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] kernel image: $KERNEL_IMAGE"
else
  echo "[warn] kernel image missing: $KERNEL_IMAGE"
  echo "[hint] the first blocker is a direct-bootable x86 kernel artifact; the generated initramfs shell flow is staged as the second milestone"
  echo "[hint] provide a bootable x86_64 kernel image via KERNEL_BOOTABLE_IMAGE_SOURCE=<path> or KERNEL_IMAGE=<path>"
  [[ "$ARTIFACTS_STRICT" == "1" ]] && exit 1
fi

if [[ -f "$KERNEL_DEBUG_ELF" ]]; then
  echo "[info] freestanding kernel ELF (debug artifact): $KERNEL_DEBUG_ELF"
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
if [[ -f "$KERNEL_IMAGE" ]]; then
  echo "[ok] x86_64 artifact staging complete in $OUT_DIR"
  echo "[next] run smoke boot: scripts/qemu-x86_64-core-smoke.sh"
else
  echo "[warn] x86_64 artifact staging is incomplete in $OUT_DIR (bootable kernel image missing)"
  echo "[next] provide KERNEL_BOOTABLE_IMAGE_SOURCE=<path> and rerun: scripts/build-qemu-x86_64-artifacts.sh"
fi
