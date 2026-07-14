#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Stage 198B1 Part A: fail closed. Historically this exited only when
# ARTIFACTS_STRICT=1, so a compile/stage failure under the default (non-strict)
# build was swallowed and a STALE kernel remained at the output path and booted
# (the exact defect that made 128-byte DebugLog behavior look current in Stage
# 198B). It now ALWAYS exits nonzero — a failed compile, link, objcopy,
# initramfs build, or copy aborts the whole build. `ARTIFACTS_STRICT` is retained
# only as an explicit override for callers that still want to force the legacy
# soft-skip (set ARTIFACTS_SOFT_FAIL=1), which nothing in the seal path does.
common_exit_if_strict_mode() {
  if [[ "${ARTIFACTS_SOFT_FAIL:-0}" == "1" ]]; then
    echo "[warn] ARTIFACTS_SOFT_FAIL=1: continuing past a build/stage failure (NOT for seals)"
    return 0
  fi
  echo "[error] build/stage step failed — failing closed (no stale artifact will be published)"
  exit 1
}

# ── Stage 198B1 Part A: fail-closed artifact build integrity ──────────────────
#
# Record a build-start stamp and DELETE the output artifacts up front, so a
# failed build leaves NO artifact at the expected output path (never accept an
# artifact merely because a file already exists). Published artifacts are later
# required to be strictly newer than this stamp.
common_build_integrity_init() {
  BUILD_START_EPOCH="$(date +%s)"
  BUILD_START_HUMAN="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  BUILD_COMMIT="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
  ARTIFACT_MANIFEST="${ARTIFACT_MANIFEST:-$OUT_DIR/artifact-manifest.txt}"
  export BUILD_START_EPOCH BUILD_START_HUMAN BUILD_COMMIT ARTIFACT_MANIFEST
  rm -f "$KERNEL_IMAGE" "$INITRAMFS_IMAGE" "$INITRAMFS_IMAGE_ABS" "$ARTIFACT_MANIFEST" 2>/dev/null || true
  [[ -n "${KERNEL_DEBUG_ELF:-}" ]] && rm -f "$KERNEL_DEBUG_ELF" 2>/dev/null || true
  echo "[integrity] build-start=$BUILD_START_HUMAN epoch=$BUILD_START_EPOCH commit=$BUILD_COMMIT out_dir=$OUT_DIR"
}

# Assert a just-published artifact exists AND is strictly newer than build-start
# (a stale artifact from a prior build has an older mtime and is rejected).
common_verify_artifact_fresh() {
  local path="$1" label="$2"
  if [[ ! -f "$path" ]]; then
    echo "[error][integrity] $label missing after build (fail closed): $path"
    return 1
  fi
  local m
  m="$(stat -c '%Y' "$path")"
  if [[ "$m" -lt "${BUILD_START_EPOCH:?build-integrity not initialized}" ]]; then
    echo "[error][integrity] $label is STALE (mtime $m < build-start $BUILD_START_EPOCH): $path"
    return 1
  fi
  echo "[integrity] fresh: $label mtime=$m > build-start=$BUILD_START_EPOCH ($path)"
}

# Verify the freshly produced kernel binary CONTAINS every required marker and
# NONE of the forbidden/obsolete ones. The untagged ordinary-cap retirement
# marker is the exact stale-kernel fingerprint (pre-198B kernels carry it).
common_verify_kernel_markers() {
  local kernel="$1" arch="$2" rc=0 m
  local required=(
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendOrdinaryCap result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendOrdinaryCapEnqueue result=ok"
    "GLOBAL_LOCK_RETIRE_CLASS_DONE arch=${arch} class=IpcSendPlain result=ok"
    "IPC_ORDINARY_CAP_OBJECT_IDENTITY"
  )
  local forbidden=(
    "GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcSendOrdinaryCap result=ok"
    "class=IpcSendReplyCapEnqueue"
    "class=IpcSendSharedRegion"
    "InitramfsReadChunk"
  )
  for m in "${required[@]}"; do
    if ! grep -qa -F -- "$m" "$kernel"; then
      echo "[error][integrity] required marker ABSENT from kernel ($arch): '$m'"
      rc=1
    fi
  done
  for m in "${forbidden[@]}"; do
    if grep -qa -F -- "$m" "$kernel"; then
      echo "[error][integrity] FORBIDDEN/obsolete marker PRESENT in kernel ($arch): '$m' (stale binary?)"
      rc=1
    fi
  done
  [[ "$rc" -eq 0 ]] && echo "[integrity] kernel marker contract satisfied ($arch): required present, forbidden absent"
  return "$rc"
}

# Record commit + per-artifact hash/size/mtime to the manifest.
common_write_manifest() {
  local arch="$1"; shift
  {
    echo "commit=$BUILD_COMMIT"
    echo "arch=$arch"
    echo "build_start=$BUILD_START_HUMAN epoch=$BUILD_START_EPOCH"
    local f
    for f in "$@"; do
      [[ -f "$f" ]] || continue
      printf 'artifact %s sha256=%s size=%s mtime=%s\n' \
        "$f" "$(sha256sum "$f" | cut -d' ' -f1)" "$(stat -c '%s' "$f")" "$(stat -c '%Y' "$f")"
    done
  } | tee "$ARTIFACT_MANIFEST"
}

# Emit the per-arch build-integrity line (aggregated by the seal runner).
common_emit_build_integrity_line() {
  local arch="$1"
  echo "ARTIFACT_BUILD_INTEGRITY arch=${arch} stale_artifact_acceptance=0 failed_build_rejected=1 result=ok"
}

common_prepare_rootfs_dirs() {
  mkdir -p "$ROOTFS_DIR" "$ROOTFS_DIR/sbin" "$ROOTFS_DIR/dev" "$ROOTFS_DIR/proc" "$ROOTFS_DIR/sys"
  mkdir -p "$(dirname "$INITRAMFS_IMAGE")"
  INITRAMFS_IMAGE_ABS="$(cd "$(dirname "$INITRAMFS_IMAGE")" && pwd)/$(basename "$INITRAMFS_IMAGE")"
}

common_stage_server_init_elf() {
  if [[ ! -f "$SERVER_ELF" ]]; then
    echo "[warn] server ELF missing: $SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$SERVER_ELF" "$ROOTFS_DIR/init"
  chmod +x "$ROOTFS_DIR/init"

  cp "$SERVER_ELF" "$ROOTFS_DIR/sbin/init_server"
  chmod +x "$ROOTFS_DIR/sbin/init_server"

  if [[ "${SERVER_BIN:-}" == "init_server" ]] && command -v strings >/dev/null 2>&1; then
    if ! strings "$SERVER_ELF" | rg -q 'INIT_IDLE_PARK_BEGIN'; then
      echo "[error] init_server ELF is missing INIT_IDLE_PARK_BEGIN; rebuild/repackage would boot stale idle-yield path: $SERVER_ELF"
      return 1
    fi
  fi

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] server ELF has forbidden RWE PT_LOAD segment: $SERVER_ELF"
      return 1
    fi
    if ! printf '%s\n' "$readelf_out" | awk '
      BEGIN { page = 4096; exec_n = 0; write_n = 0; }
      $1 == "LOAD" {
        vaddr = strtonum("0x" $3);
        memsz = strtonum("0x" $6);
        flg = $7;
        start = int(vaddr / page);
        end = int((vaddr + memsz - 1) / page);
        if (memsz == 0) next;
        if (index(flg, "E") > 0) {
          exec_start[exec_n] = start;
          exec_end[exec_n] = end;
          exec_n++;
        }
        if (index(flg, "W") > 0) {
          write_start[write_n] = start;
          write_end[write_n] = end;
          write_n++;
        }
      }
      END {
        for (i = 0; i < exec_n; i++) {
          for (j = 0; j < write_n; j++) {
            if (!(exec_end[i] < write_start[j] || write_end[j] < exec_start[i])) {
              exit 1;
            }
          }
        }
        exit 0;
      }
    '; then
      echo "[error] server ELF has executable/writable PT_LOAD page overlap: $SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $SERVER_ELF"
  fi

  echo "[ok] staged server ELF as /init and /sbin/init_server"
}

common_stage_aux_server_elf() {
  if [[ ! -f "$PM_ELF" ]]; then
    echo "[warn] aux server ELF missing: $PM_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$PM_ELF" "$ROOTFS_DIR/sbin/process_manager"
  chmod +x "$ROOTFS_DIR/sbin/process_manager"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$PM_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] aux server ELF has forbidden RWE PT_LOAD segment: $PM_ELF"
      return 1
    fi
    if ! printf '%s\n' "$readelf_out" | awk '
      BEGIN { page = 4096; exec_n = 0; write_n = 0; }
      $1 == "LOAD" {
        vaddr = strtonum("0x" $3);
        memsz = strtonum("0x" $6);
        flg = $7;
        start = int(vaddr / page);
        end = int((vaddr + memsz - 1) / page);
        if (memsz == 0) next;
        if (index(flg, "E") > 0) {
          exec_start[exec_n] = start;
          exec_end[exec_n] = end;
          exec_n++;
        }
        if (index(flg, "W") > 0) {
          write_start[write_n] = start;
          write_end[write_n] = end;
          write_n++;
        }
      }
      END {
        for (i = 0; i < exec_n; i++) {
          for (j = 0; j < write_n; j++) {
            if (!(exec_end[i] < write_start[j] || write_end[j] < exec_start[i])) {
              exit 1;
            }
          }
        }
        exit 0;
      }
    '; then
      echo "[error] aux server ELF has executable/writable PT_LOAD page overlap: $PM_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $PM_ELF"
  fi

  echo "[ok] staged aux server ELF as /sbin/process_manager"
}

common_stage_supervisor_elf() {
  if [[ ! -f "$SUPERVISOR_ELF" ]]; then
    echo "[warn] supervisor ELF missing: $SUPERVISOR_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$SUPERVISOR_ELF" "$ROOTFS_DIR/sbin/supervisor"
  chmod +x "$ROOTFS_DIR/sbin/supervisor"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$SUPERVISOR_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] supervisor ELF has forbidden RWE PT_LOAD segment: $SUPERVISOR_ELF"
      return 1
    fi
    if ! printf '%s\n' "$readelf_out" | awk '
      BEGIN { page = 4096; exec_n = 0; write_n = 0; }
      $1 == "LOAD" {
        vaddr = strtonum("0x" $3);
        memsz = strtonum("0x" $6);
        flg = $7;
        start = int(vaddr / page);
        end = int((vaddr + memsz - 1) / page);
        if (memsz == 0) next;
        if (index(flg, "E") > 0) {
          exec_start[exec_n] = start;
          exec_end[exec_n] = end;
          exec_n++;
        }
        if (index(flg, "W") > 0) {
          write_start[write_n] = start;
          write_end[write_n] = end;
          write_n++;
        }
      }
      END {
        for (i = 0; i < exec_n; i++) {
          for (j = 0; j < write_n; j++) {
            if (!(exec_end[i] < write_start[j] || write_end[j] < exec_start[i])) {
              exit 1;
            }
          }
        }
        exit 0;
      }
    '; then
      echo "[error] supervisor ELF has executable/writable PT_LOAD page overlap: $SUPERVISOR_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $SUPERVISOR_ELF"
  fi

  echo "[ok] staged supervisor ELF as /sbin/supervisor"
}

common_stage_initramfs_server_elf() {
  if [[ ! -f "$INITRAMFS_SERVER_ELF" ]]; then
    echo "[warn] initramfs server ELF missing: $INITRAMFS_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$INITRAMFS_SERVER_ELF" "$ROOTFS_DIR/sbin/initramfs_srv"
  chmod +x "$ROOTFS_DIR/sbin/initramfs_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$INITRAMFS_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] initramfs server ELF has forbidden RWE PT_LOAD segment: $INITRAMFS_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $INITRAMFS_SERVER_ELF"
  fi

  echo "[ok] staged initramfs server ELF as /sbin/initramfs_srv"
}

common_stage_devfs_server_elf() {
  if [[ ! -f "$DEVFS_SERVER_ELF" ]]; then
    echo "[warn] devfs server ELF missing: $DEVFS_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$DEVFS_SERVER_ELF" "$ROOTFS_DIR/sbin/devfs_srv"
  chmod +x "$ROOTFS_DIR/sbin/devfs_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$DEVFS_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] devfs server ELF has forbidden RWE PT_LOAD segment: $DEVFS_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $DEVFS_SERVER_ELF"
  fi

  echo "[ok] staged devfs server ELF as /sbin/devfs_srv"
}

common_stage_vfs_server_elf() {
  if [[ ! -f "$VFS_SERVER_ELF" ]]; then
    echo "[warn] vfs server ELF missing: $VFS_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$VFS_SERVER_ELF" "$ROOTFS_DIR/sbin/vfs_server"
  chmod +x "$ROOTFS_DIR/sbin/vfs_server"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$VFS_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] vfs server ELF has forbidden RWE PT_LOAD segment: $VFS_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $VFS_SERVER_ELF"
  fi

  echo "[ok] staged vfs server ELF as /sbin/vfs_server"
}

common_stage_driver_manager_elf() {
  if [[ ! -f "$DRIVER_MANAGER_ELF" ]]; then
    echo "[warn] driver manager ELF missing: $DRIVER_MANAGER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$DRIVER_MANAGER_ELF" "$ROOTFS_DIR/sbin/driver_manager"
  chmod +x "$ROOTFS_DIR/sbin/driver_manager"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$DRIVER_MANAGER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] driver manager ELF has forbidden RWE PT_LOAD segment: $DRIVER_MANAGER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $DRIVER_MANAGER_ELF"
  fi

  echo "[ok] staged driver manager ELF as /sbin/driver_manager"
}

common_stage_blkcache_server_elf() {
  if [[ ! -f "$BLKCACHE_SERVER_ELF" ]]; then
    echo "[warn] blkcache server ELF missing: $BLKCACHE_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$BLKCACHE_SERVER_ELF" "$ROOTFS_DIR/sbin/blkcache_srv"
  chmod +x "$ROOTFS_DIR/sbin/blkcache_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$BLKCACHE_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] blkcache server ELF has forbidden RWE PT_LOAD segment: $BLKCACHE_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $BLKCACHE_SERVER_ELF"
  fi

  echo "[ok] staged blkcache server ELF as /sbin/blkcache_srv"
}


common_stage_virtio_blk_server_elf() {
  if [[ ! -f "$VIRTIO_BLK_SERVER_ELF" ]]; then
    echo "[warn] virtio blk server ELF missing: $VIRTIO_BLK_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$VIRTIO_BLK_SERVER_ELF" "$ROOTFS_DIR/sbin/virtio_blk_srv"
  chmod +x "$ROOTFS_DIR/sbin/virtio_blk_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$VIRTIO_BLK_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] virtio blk server ELF has forbidden RWE PT_LOAD segment: $VIRTIO_BLK_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $VIRTIO_BLK_SERVER_ELF"
  fi

  echo "[ok] staged virtio blk server ELF as /sbin/virtio_blk_srv"
}

common_stage_ramfs_server_elf() {
  if [[ ! -f "$RAMFS_SERVER_ELF" ]]; then
    echo "[warn] ramfs server ELF missing: $RAMFS_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$RAMFS_SERVER_ELF" "$ROOTFS_DIR/sbin/ramfs_srv"
  chmod +x "$ROOTFS_DIR/sbin/ramfs_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$RAMFS_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] ramfs server ELF has forbidden RWE PT_LOAD segment: $RAMFS_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $RAMFS_SERVER_ELF"
  fi

  echo "[ok] staged ramfs server ELF as /sbin/ramfs_srv"
}

common_stage_fat_server_elf() {
  if [[ ! -f "$FAT_SERVER_ELF" ]]; then
    echo "[warn] fat server ELF missing: $FAT_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$FAT_SERVER_ELF" "$ROOTFS_DIR/sbin/fat_srv"
  chmod +x "$ROOTFS_DIR/sbin/fat_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$FAT_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] fat server ELF has forbidden RWE PT_LOAD segment: $FAT_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $FAT_SERVER_ELF"
  fi

  echo "[ok] staged fat server ELF as /sbin/fat_srv"
}

common_stage_ext4_server_elf() {
  if [[ ! -f "$EXT4_SERVER_ELF" ]]; then
    echo "[warn] ext4 server ELF missing: $EXT4_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$EXT4_SERVER_ELF" "$ROOTFS_DIR/sbin/ext4_srv"
  chmod +x "$ROOTFS_DIR/sbin/ext4_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$EXT4_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] ext4 server ELF has forbidden RWE PT_LOAD segment: $EXT4_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $EXT4_SERVER_ELF"
  fi

  echo "[ok] staged ext4 server ELF as /sbin/ext4_srv"
}

common_supervisor_restart_test_enabled() {
  [[ "${YARM_SUPERVISOR_RESTART_TEST:-${SUPERVISOR_RESTART_TEST:-0}}" == "1" ]]
}

common_stage_crash_test_server_elf() {
  if ! common_supervisor_restart_test_enabled; then
    echo "[info] CRASH_TEST_IMAGE_GATED: supervisor restart test disabled; not staging /sbin/crash_test_srv"
    return 0
  fi

  if [[ ! -f "$CRASH_TEST_SERVER_ELF" ]]; then
    echo "[warn] crash_test server ELF missing: $CRASH_TEST_SERVER_ELF"
    common_exit_if_strict_mode
    return 1
  fi

  cp "$CRASH_TEST_SERVER_ELF" "$ROOTFS_DIR/sbin/crash_test_srv"
  chmod +x "$ROOTFS_DIR/sbin/crash_test_srv"

  if command -v readelf >/dev/null 2>&1; then
    local readelf_out
    readelf_out="$(readelf -W -l "$CRASH_TEST_SERVER_ELF")"
    if printf '%s\n' "$readelf_out" | rg -q 'LOAD\s+.*RWE'; then
      echo "[error] crash_test_srv ELF has forbidden RWE PT_LOAD segment: $CRASH_TEST_SERVER_ELF"
      return 1
    fi
  else
    echo "[warn] readelf not found; skipping PT_LOAD RWE check for $CRASH_TEST_SERVER_ELF"
  fi

  echo "[ok] staged crash_test_srv ELF as /sbin/crash_test_srv"
  echo "CRASH_TEST_IMAGE_ID_ASSIGNED image_id=13"
}

common_verify_initramfs_stage_paths() {
  local missing=0
  local required_paths=("init" "sbin/init_server" "sbin/initramfs_srv" "sbin/devfs_srv" "sbin/vfs_server" "sbin/driver_manager" "sbin/blkcache_srv" "sbin/virtio_blk_srv" "sbin/process_manager" "sbin/supervisor" "sbin/ramfs_srv" "sbin/fat_srv" "sbin/ext4_srv")
  if common_supervisor_restart_test_enabled; then
    required_paths+=("sbin/crash_test_srv")
  fi
  local stale=0
  for path in "${required_paths[@]}"; do
    if [[ ! -f "$ROOTFS_DIR/$path" ]]; then
      echo "[error] expected initramfs path missing: $ROOTFS_DIR/$path"
      missing=1
      continue
    fi
    # Stage 198B1 Part A: reject a STALE staged artifact (a server compile that
    # failed under `|| true` would leave the previous build's ELF here). Every
    # staged path must have been (re)written this build, i.e. mtime >= build-start.
    if [[ -n "${BUILD_START_EPOCH:-}" ]]; then
      local m
      m="$(stat -c '%Y' "$ROOTFS_DIR/$path")"
      if [[ "$m" -lt "$BUILD_START_EPOCH" ]]; then
        echo "[error][integrity] STALE staged artifact (mtime $m < build-start $BUILD_START_EPOCH): $ROOTFS_DIR/$path"
        stale=1
      fi
    fi
  done
  if [[ "$missing" -ne 0 || "$stale" -ne 0 ]]; then
    echo "[error] initramfs staging incomplete or stale"
    common_exit_if_strict_mode
    return 1
  fi
  echo "[ok] all required initramfs stage paths present and fresh"
}

common_create_initramfs_newc() {
  if ! command -v cpio >/dev/null 2>&1; then
    echo "[warn] cpio not found; creating placeholder initramfs archive file"
    : > "$INITRAMFS_IMAGE_ABS"
    common_exit_if_strict_mode
    return
  fi

  local cpio_help
  cpio_help="$(cpio --help 2>&1 || true)"
  if printf '%s' "$cpio_help" | rg -q -- '--null'; then
    ( cd "$ROOTFS_DIR" && find . -print0 | cpio --null -ov --format=newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
  elif printf '%s' "$cpio_help" | rg -q -- ' -H '; then
    ( cd "$ROOTFS_DIR" && find . -print | cpio -o -H newc > "$INITRAMFS_IMAGE_ABS" ) >/dev/null
  else
    echo "[warn] cpio lacks required newc flags; creating placeholder initramfs archive file"
    : > "$INITRAMFS_IMAGE_ABS"
    common_exit_if_strict_mode
  fi
}

# common_create_initramfs_aligned — CPIO newc packer with mandatory ELF alignment.
#
# Uses scripts/pack-initramfs-aligned.py to align every ELF payload in the
# archive. This includes /init, early services, late services, and every other
# ELF staged below /sbin. The packer emits one ALIGN_PROOF line per ELF and
# exits non-zero if any payload is not 4096-byte aligned.
common_create_initramfs_aligned() {
  local packer
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  packer="${script_dir}/../pack-initramfs-aligned.py"

  if ! command -v python3 >/dev/null 2>&1; then
    echo "[error] python3 is required for mandatory initramfs ELF alignment"
    return 1
  fi

  if [[ ! -f "$packer" ]]; then
    echo "[error] mandatory aligned initramfs packer not found at $packer"
    return 1
  fi

  echo "[info] packing initramfs with mandatory 4096-byte alignment for every ELF"
  local pack_log
  pack_log="$(python3 "$packer" "$ROOTFS_DIR" "$INITRAMFS_IMAGE_ABS" 2>&1)" || {
    echo "[error] pack-initramfs-aligned.py failed"
    printf '%s\n' "$pack_log" | sed 's/^/  /'
    return 1
  }
  printf '%s\n' "$pack_log" | sed 's/^/[initramfs-pack] /'
  echo "[ok] aligned initramfs archive created: $INITRAMFS_IMAGE_ABS"
}
