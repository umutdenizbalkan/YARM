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
