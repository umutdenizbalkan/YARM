#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

common_exit_if_strict_mode() {
  if [[ "${ARTIFACTS_STRICT:-0}" == "1" ]]; then
    exit 1
  fi
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

common_verify_initramfs_stage_paths() {
  local missing=0
  for path in "init" "sbin/init_server" "sbin/initramfs_srv" "sbin/devfs_srv" "sbin/vfs_server" "sbin/driver_manager" "sbin/blkcache_srv" "sbin/virtio_blk_srv" "sbin/process_manager" "sbin/supervisor"; do
    if [[ ! -f "$ROOTFS_DIR/$path" ]]; then
      echo "[error] expected initramfs path missing: $ROOTFS_DIR/$path"
      missing=1
    fi
  done
  if [[ "$missing" -ne 0 ]]; then
    echo "[error] initramfs staging incomplete"
    common_exit_if_strict_mode
    return 1
  fi
  echo "[ok] all required initramfs stage paths present"
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
