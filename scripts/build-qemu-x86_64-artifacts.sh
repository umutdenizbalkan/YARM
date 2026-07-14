#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

source "$(dirname "$0")/lib/build-qemu-artifacts-common.sh"

OUT_DIR=${OUT_DIR:-build-x86_64}
ROOTFS_DIR=${ROOTFS_DIR:-$OUT_DIR/rootfs}

KERNEL_RUST_TARGET=${KERNEL_RUST_TARGET:-targets/x86_64-yarm-none.json}
KERNEL_RUST_TARGET_DIR=${KERNEL_RUST_TARGET_DIR:-x86_64-yarm-none}
SERVER_RUST_TARGET=${SERVER_RUST_TARGET:-targets/x86_64-yarm-user-none.json}
SERVER_RUST_TARGET_DIR=${SERVER_RUST_TARGET_DIR:-x86_64-yarm-user-none}

SERVER_BIN=${SERVER_BIN:-init_server}
PM_BIN=${PM_BIN:-process_manager}
SUPERVISOR_BIN=${SUPERVISOR_BIN:-supervisor}
INITRAMFS_SERVER_BIN=${INITRAMFS_SERVER_BIN:-initramfs_srv}
DEVFS_SERVER_BIN=${DEVFS_SERVER_BIN:-devfs_srv}
VFS_SERVER_BIN=${VFS_SERVER_BIN:-vfs_server}
BLKCACHE_SERVER_BIN=${BLKCACHE_SERVER_BIN:-blkcache_srv}
VIRTIO_BLK_SERVER_BIN=${VIRTIO_BLK_SERVER_BIN:-virtio_blk_srv}
DRIVER_MANAGER_BIN=${DRIVER_MANAGER_BIN:-driver_manager}
RAMFS_SERVER_BIN=${RAMFS_SERVER_BIN:-ramfs_srv}
FAT_SERVER_BIN=${FAT_SERVER_BIN:-fat_srv}
EXT4_SERVER_BIN=${EXT4_SERVER_BIN:-ext4_srv}
CRASH_TEST_SERVER_BIN=${CRASH_TEST_SERVER_BIN:-crash_test_srv}
KERNEL_BIN=${KERNEL_BIN:-kernel_boot}

SERVER_PACKAGE=${SERVER_PACKAGE:-yarm-control-plane-servers}
INITRAMFS_SERVER_PACKAGE=${INITRAMFS_SERVER_PACKAGE:-yarm-fs-servers}
KERNEL_PACKAGE=${KERNEL_PACKAGE:-yarm}

SERVER_BUILD_PROFILE=${SERVER_BUILD_PROFILE:-x86-none}

SERVER_ELF=${SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${SERVER_BIN}}
PM_ELF=${PM_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${PM_BIN}}
SUPERVISOR_ELF=${SUPERVISOR_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${SUPERVISOR_BIN}}
INITRAMFS_SERVER_ELF=${INITRAMFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${INITRAMFS_SERVER_BIN}}
DEVFS_SERVER_ELF=${DEVFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${DEVFS_SERVER_BIN}}
VFS_SERVER_ELF=${VFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${VFS_SERVER_BIN}}
BLKCACHE_SERVER_ELF=${BLKCACHE_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${BLKCACHE_SERVER_BIN}}
VIRTIO_BLK_SERVER_ELF=${VIRTIO_BLK_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${VIRTIO_BLK_SERVER_BIN}}
DRIVER_MANAGER_ELF=${DRIVER_MANAGER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${DRIVER_MANAGER_BIN}}
RAMFS_SERVER_ELF=${RAMFS_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${RAMFS_SERVER_BIN}}
FAT_SERVER_ELF=${FAT_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${FAT_SERVER_BIN}}
EXT4_SERVER_ELF=${EXT4_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${EXT4_SERVER_BIN}}
CRASH_TEST_SERVER_ELF=${CRASH_TEST_SERVER_ELF:-target/${SERVER_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${CRASH_TEST_SERVER_BIN}}
KERNEL_ELF=${KERNEL_ELF:-target/${KERNEL_RUST_TARGET_DIR}/${SERVER_BUILD_PROFILE}/${KERNEL_BIN}}

INITRAMFS_IMAGE=${INITRAMFS_IMAGE:-$OUT_DIR/initramfs-core.cpio}
KERNEL_IMAGE=${KERNEL_IMAGE:-$OUT_DIR/kernel_boot.elf}
KERNEL_DEBUG_ELF=${KERNEL_DEBUG_ELF:-$OUT_DIR/${KERNEL_BIN}.debug.elf}

ARTIFACTS_STRICT=${ARTIFACTS_STRICT:-0}
BOOTSTRAP_FEATURE_ARGS=${BOOTSTRAP_FEATURE_ARGS:---no-default-features}
BUILD_STD_COMPONENTS=${BUILD_STD_COMPONENTS:-core,alloc,compiler_builtins,panic_abort}
BUILD_LOG=${BUILD_LOG:-$OUT_DIR/x86_64-build.log}

mkdir -p "$OUT_DIR"
common_prepare_rootfs_dirs
# Stage 198B1 Part A: record build-start + delete stale outputs (fail closed).
common_build_integrity_init

CARGO_Z_ARGS=()
if cargo -V 2>/dev/null | grep -qi nightly; then
  CARGO_Z_ARGS=(-Z "build-std=${BUILD_STD_COMPONENTS}")
  if [[ "$KERNEL_RUST_TARGET" == *.json || "$SERVER_RUST_TARGET" == *.json ]]; then
    CARGO_Z_ARGS+=(-Z "json-target-spec")
  fi
else
  echo "[warn] selected cargo is not nightly; building without -Z build-std/json-target-spec"
fi

echo "[info] building ${KERNEL_PACKAGE}/${KERNEL_BIN} for ${KERNEL_RUST_TARGET}"
# Stage 198B1 Part A: NO `set +e` — `set -euo pipefail` (with the `| tee` under
# pipefail) makes every cargo build fatal; a compile/link failure aborts here
# and NO artifact is published (the stale output was already deleted).
cargo build \
  --target "$KERNEL_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$KERNEL_PACKAGE" \
  --bin "$KERNEL_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee "$BUILD_LOG"
KERNEL_BUILD_STATUS=$?

echo "[info] building ${SERVER_PACKAGE}/${SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$SERVER_PACKAGE" \
  --bin "$SERVER_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
SERVER_BUILD_STATUS=$?

echo "[info] building ${SERVER_PACKAGE}/${PM_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$SERVER_PACKAGE" \
  --bin "$PM_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
PM_BUILD_STATUS=$?

echo "[info] building ${SERVER_PACKAGE}/${SUPERVISOR_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$SERVER_PACKAGE" \
  --bin "$SUPERVISOR_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
SUPERVISOR_BUILD_STATUS=$?

echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${INITRAMFS_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$INITRAMFS_SERVER_PACKAGE" \
  --bin "$INITRAMFS_SERVER_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
INITRAMFS_SERVER_BUILD_STATUS=$?

echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${DEVFS_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$INITRAMFS_SERVER_PACKAGE" \
  --bin "$DEVFS_SERVER_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
DEVFS_SERVER_BUILD_STATUS=$?

echo "[info] building ${SERVER_PACKAGE}/${VFS_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$SERVER_PACKAGE" \
  --bin "$VFS_SERVER_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
VFS_SERVER_BUILD_STATUS=$?
echo "[info] building ${SERVER_PACKAGE}/${DRIVER_MANAGER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build \
  --target "$SERVER_RUST_TARGET" \
  --profile "$SERVER_BUILD_PROFILE" \
  ${BOOTSTRAP_FEATURE_ARGS} \
  -p "$SERVER_PACKAGE" \
  --bin "$DRIVER_MANAGER_BIN" \
  "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
DRIVER_MANAGER_BUILD_STATUS=$?
echo "[info] building yarm-driver-servers/${BLKCACHE_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p yarm-driver-servers --bin "$BLKCACHE_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
BLKCACHE_SERVER_BUILD_STATUS=$?
echo "[info] building yarm-driver-servers/${VIRTIO_BLK_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p yarm-driver-servers --bin "$VIRTIO_BLK_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
VIRTIO_BLK_SERVER_BUILD_STATUS=$?
echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${RAMFS_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$INITRAMFS_SERVER_PACKAGE" --bin "$RAMFS_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
RAMFS_SERVER_BUILD_STATUS=$?
echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${FAT_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$INITRAMFS_SERVER_PACKAGE" --bin "$FAT_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
FAT_SERVER_BUILD_STATUS=$?
echo "[info] building ${INITRAMFS_SERVER_PACKAGE}/${EXT4_SERVER_BIN} for ${SERVER_RUST_TARGET}" | tee -a "$BUILD_LOG"
cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$INITRAMFS_SERVER_PACKAGE" --bin "$EXT4_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
EXT4_SERVER_BUILD_STATUS=$?
if common_supervisor_restart_test_enabled; then
  echo "[info] building ${SERVER_PACKAGE}/${CRASH_TEST_SERVER_BIN} for ${SERVER_RUST_TARGET} (supervisor restart test)" | tee -a "$BUILD_LOG"
  cargo build --target "$SERVER_RUST_TARGET" --profile "$SERVER_BUILD_PROFILE" ${BOOTSTRAP_FEATURE_ARGS} -p "$SERVER_PACKAGE" --bin "$CRASH_TEST_SERVER_BIN" "${CARGO_Z_ARGS[@]}" 2>&1 | tee -a "$BUILD_LOG"
  CRASH_TEST_SERVER_BUILD_STATUS=$?
else
  CRASH_TEST_SERVER_BUILD_STATUS=0
fi
# Stage 198B1 Part A: every cargo build above ran fail-closed under
# `set -euo pipefail`; reaching here means all of them succeeded.

# Staging: a MISSING ELF fails closed (the helpers call common_exit_if_strict_mode,
# which now always `exit`s — uncatchable by `|| true`). `|| true` is retained ONLY
# so the pre-existing advisory ELF-hygiene checks (RWE / W^X page-overlap) stay
# advisory exactly as before — those are NOT build/compile/copy failures and are
# out of scope for Stage 198B1. The fatal freshness+existence gate below
# (common_verify_initramfs_stage_paths) is what actually rejects a missing/stale
# staged artifact, so a failed server compile still fails the build closed.
common_stage_server_init_elf || true
common_stage_aux_server_elf || true
common_stage_supervisor_elf || true
common_stage_initramfs_server_elf || true
common_stage_devfs_server_elf || true
common_stage_vfs_server_elf || true
common_stage_blkcache_server_elf || true
common_stage_virtio_blk_server_elf || true
common_stage_driver_manager_elf || true
common_stage_ramfs_server_elf || true
common_stage_fat_server_elf || true
common_stage_ext4_server_elf || true
common_stage_crash_test_server_elf || true
# FATAL gate: every required initramfs path must exist AND be fresh (mtime newer
# than build-start) — a missing/stale staged ELF aborts here.
common_verify_initramfs_stage_paths
# Phase 3B: use 4096-byte-aligned packer so late-service ELFs can be zero-copy loaded.
common_create_initramfs_aligned

# Stage 198B1 Part A: publish the kernel image atomically (stage to a temp
# sibling, then mv). `set -e` guarantees a failed cp aborts.
if [[ ! -f "$KERNEL_ELF" ]]; then
  echo "[error][integrity] kernel ELF not produced by the build: $KERNEL_ELF"
  exit 1
fi
cp "$KERNEL_ELF" "${KERNEL_IMAGE}.staging.$$"
mv -f "${KERNEL_IMAGE}.staging.$$" "$KERNEL_IMAGE"
cp "$KERNEL_ELF" "${KERNEL_DEBUG_ELF}.staging.$$"
mv -f "${KERNEL_DEBUG_ELF}.staging.$$" "$KERNEL_DEBUG_ELF"

# Freshness + marker + manifest gates (fail closed on any violation).
common_verify_artifact_fresh "$KERNEL_IMAGE" "kernel image"
common_verify_artifact_fresh "$INITRAMFS_IMAGE_ABS" "initramfs image"
common_verify_kernel_markers "$KERNEL_IMAGE" "x86_64"
common_write_manifest "x86_64" "$KERNEL_IMAGE" "$INITRAMFS_IMAGE_ABS"
common_emit_build_integrity_line "x86_64"

echo "[ok] initramfs image: $INITRAMFS_IMAGE_ABS"
echo "[ok] kernel image: $KERNEL_IMAGE"
[[ -f "$KERNEL_DEBUG_ELF" ]] && echo "[ok] kernel debug elf: $KERNEL_DEBUG_ELF"

echo "[ok] x86_64 qemu artifacts completed"
echo "[next] run smoke boot: scripts/qemu-x86_64-core-smoke.sh"
