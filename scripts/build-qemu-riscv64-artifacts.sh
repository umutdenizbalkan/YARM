#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

source "$(dirname "$0")/lib/build-qemu-artifacts-common.sh"

OUT_DIR=${OUT_DIR:-build-riscv64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}
KERNEL_RUST_TARGET=${KERNEL_RUST_TARGET:-riscv64gc-unknown-none-elf}
KERNEL_RUST_TARGET_DIR=${KERNEL_RUST_TARGET_DIR:-riscv64gc-unknown-none-elf}
SERVER_RUST_TARGET=${SERVER_RUST_TARGET:-targets/riscv64-yarm-user-none.json}
SERVER_RUST_TARGET_DIR=${SERVER_RUST_TARGET_DIR:-riscv64-yarm-user-none}
SERVER_BIN=${SERVER_BIN:-init_server}
PM_BIN=${PM_BIN:-process_manager}
SUPERVISOR_BIN=${SUPERVISOR_BIN:-supervisor}
INITRAMFS_SERVER_BIN=${INITRAMFS_SERVER_BIN:-initramfs_srv}
DEVFS_SERVER_BIN=${DEVFS_SERVER_BIN:-devfs_srv}
VFS_SERVER_BIN=${VFS_SERVER_BIN:-vfs_server}
BLKCACHE_SERVER_BIN=${BLKCACHE_SERVER_BIN:-blkcache_srv}
VIRTIO_BLK_SERVER_BIN=${VIRTIO_BLK_SERVER_BIN:-virtio_blk_srv}
DRIVER_MANAGER_BIN=${DRIVER_MANAGER_BIN:-driver_manager}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}
SERVER_PACKAGE=${SERVER_PACKAGE:-yarm-control-plane-servers}
INITRAMFS_SERVER_PACKAGE=${INITRAMFS_SERVER_PACKAGE:-yarm-fs-servers}
KERNEL_PACKAGE=${KERNEL_PACKAGE:-yarm}
SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-release}
SERVER_ELF=${SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
PM_ELF=${PM_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${PM_BIN}}
SUPERVISOR_ELF=${SUPERVISOR_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${SUPERVISOR_BIN}}
INITRAMFS_SERVER_ELF=${INITRAMFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${INITRAMFS_SERVER_BIN}}
DEVFS_SERVER_ELF=${DEVFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${DEVFS_SERVER_BIN}}
VFS_SERVER_ELF=${VFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${VFS_SERVER_BIN}}
BLKCACHE_SERVER_ELF=${BLKCACHE_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${BLKCACHE_SERVER_BIN}}
VIRTIO_BLK_SERVER_ELF=${VIRTIO_BLK_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${VIRTIO_BLK_SERVER_BIN}}
DRIVER_MANAGER_ELF=${DRIVER_MANAGER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${DRIVER_MANAGER_BIN}}
KERNEL_ELF=${KERNEL_ELF:-target/${KERNEL_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}
INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/yarm-riscv64.bin}
ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}

mkdir -p "$OUT_DIR"
common_prepare_rootfs_dirs

CARGO_Z_ARGS=()
if cargo -V 2>/dev/null | rg -q "nightly"; then
  CARGO_Z_ARGS=(-Z "build-std=${BUILD_STD_COMPONENTS}")
  [[ "$SERVER_RUST_TARGET" == *.json || "$KERNEL_RUST_TARGET" == *.json ]] && CARGO_Z_ARGS+=(-Z "json-target-spec")
fi

echo "[info] building ${KERNEL_PACKAGE}/${KERNEL_BIN} for ${KERNEL_RUST_TARGET} and ${SERVER_PACKAGE}/${SERVER_BIN} for ${SERVER_RUST_TARGET}"
set +e
cargo build --target "$KERNEL_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$KERNEL_PACKAGE" --bin "$KERNEL_BIN" "${CARGO_Z_ARGS[@]}"
KERNEL_BUILD_STATUS=$?
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$SERVER_BIN" "${CARGO_Z_ARGS[@]}"
SERVER_BUILD_STATUS=$?
echo "[info] building ${SERVER_PACKAGE}/${PM_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$PM_BIN" "${CARGO_Z_ARGS[@]}"
PM_BUILD_STATUS=$?
echo "[info] building ${SERVER_PACKAGE}/${SUPERVISOR_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$SUPERVISOR_BIN" "${CARGO_Z_ARGS[@]}"
SUPERVISOR_BUILD_STATUS=$?
echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${INITRAMFS_SERVER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$INITRAMFS_SERVER_PACKAGE" --bin "$INITRAMFS_SERVER_BIN" "${CARGO_Z_ARGS[@]}"
INITRAMFS_SERVER_BUILD_STATUS=$?
echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${DEVFS_SERVER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$INITRAMFS_SERVER_PACKAGE" --bin "$DEVFS_SERVER_BIN" "${CARGO_Z_ARGS[@]}"
DEVFS_SERVER_BUILD_STATUS=$?
echo "[info] building ${SERVER_PACKAGE}/${VFS_SERVER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$VFS_SERVER_BIN" "${CARGO_Z_ARGS[@]}"
VFS_SERVER_BUILD_STATUS=$?
echo "[info] building yarm-driver-servers/${BLKCACHE_SERVER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p yarm-driver-servers --bin "$BLKCACHE_SERVER_BIN" "${CARGO_Z_ARGS[@]}"
BLKCACHE_SERVER_BUILD_STATUS=$?
echo "[info] building yarm-driver-servers/${VIRTIO_BLK_SERVER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p yarm-driver-servers --bin "$VIRTIO_BLK_SERVER_BIN" "${CARGO_Z_ARGS[@]}"
VIRTIO_BLK_SERVER_BUILD_STATUS=$?
echo "[info] building ${SERVER_PACKAGE}/${DRIVER_MANAGER_BIN} for ${SERVER_RUST_TARGET}"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$DRIVER_MANAGER_BIN" "${CARGO_Z_ARGS[@]}"
DRIVER_MANAGER_BUILD_STATUS=$?
set -e
[[ "$KERNEL_BUILD_STATUS" -ne 0 || "$SERVER_BUILD_STATUS" -ne 0 || "$PM_BUILD_STATUS" -ne 0 || "$SUPERVISOR_BUILD_STATUS" -ne 0 || "$INITRAMFS_SERVER_BUILD_STATUS" -ne 0 || "$DEVFS_SERVER_BUILD_STATUS" -ne 0 || "$VFS_SERVER_BUILD_STATUS" -ne 0 || "$BLKCACHE_SERVER_BUILD_STATUS" -ne 0 || "$VIRTIO_BLK_SERVER_BUILD_STATUS" -ne 0 || "$DRIVER_MANAGER_BUILD_STATUS" -ne 0 ]] && common_exit_if_strict_mode

common_stage_server_init_elf || true
common_stage_aux_server_elf || true
common_stage_supervisor_elf || true
common_stage_initramfs_server_elf || true
common_stage_devfs_server_elf || true
common_stage_vfs_server_elf || true
common_stage_blkcache_server_elf || true
common_stage_virtio_blk_server_elf || true
common_stage_driver_manager_elf || true
common_verify_initramfs_stage_paths || true
common_create_initramfs_aligned

if [[ -f "$KERNEL_ELF" ]]; then
  if command -v llvm-objcopy >/dev/null 2>&1; then
    llvm-objcopy -O binary "$KERNEL_ELF" "$KERNEL_IMAGE" || true
  elif command -v rust-objcopy >/dev/null 2>&1; then
    rust-objcopy -O binary "$KERNEL_ELF" "$KERNEL_IMAGE" || true
  fi
fi

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
[[ -f "$KERNEL_IMAGE" ]] && echo "[ok] kernel image: $KERNEL_IMAGE" || echo "[warn] kernel image missing: $KERNEL_IMAGE"
