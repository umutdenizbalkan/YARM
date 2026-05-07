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

  echo "[info] /init staging source: ${SERVER_ELF}"
  echo "[info] /init identity: ${SERVER_BIN:-<unknown-bin>} (expected first user task binary)"
  if command -v readelf >/dev/null 2>&1; then
    local elf_type
    elf_type="$(readelf -h "$SERVER_ELF" 2>/dev/null | awk -F: '/Type:/{gsub(/^[ \t]+/,"",$2); print $2; exit}')"
    [[ -n "${elf_type:-}" ]] && echo "[info] /init ELF header type: ${elf_type}"
  fi
  if command -v strings >/dev/null 2>&1; then
    if strings "$SERVER_ELF" | rg -q "run_init_server|init_server"; then
      echo "[info] /init identity hint: control-plane init_server symbols detected"
    fi
  fi
  echo "[ok] staged server ELF as /init and /sbin/init_server"
}

common_stage_aux_server_elf() {
  local aux_elf="$1"
  local aux_label="$2"
  local aux_dest_rel="$3"
  local aux_dest_abs="$ROOTFS_DIR/$aux_dest_rel"

  if [[ ! -f "$aux_elf" ]]; then
    echo "[warn] ${aux_label} ELF missing: $aux_elf"
    common_exit_if_strict_mode
    return 1
  fi

  mkdir -p "$(dirname "$aux_dest_abs")"
  cp "$aux_elf" "$aux_dest_abs"
  chmod +x "$aux_dest_abs"

  if command -v readelf >/dev/null 2>&1; then
    local aux_type
    aux_type="$(readelf -h "$aux_elf" 2>/dev/null | awk -F: '/Type:/{gsub(/^[ \t]+/,"",$2); print $2; exit}')"
    [[ -n "${aux_type:-}" ]] && echo "[info] ${aux_label} ELF header type: ${aux_type}"
  else
    echo "[warn] readelf not found; skipping ${aux_label} identity hint"
  fi

  if command -v strings >/dev/null 2>&1; then
    if strings "$aux_elf" | rg -q "run_initramfs|initramfs_srv"; then
      echo "[info] ${aux_label} identity hint: initramfs symbols detected"
    fi
  else
    echo "[warn] strings not found; skipping ${aux_label} symbol hint"
  fi

  echo "[ok] staged ${aux_label} ELF as /${aux_dest_rel}"
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

common_verify_initramfs_stage_paths() {
  if [[ ! -f "$INITRAMFS_IMAGE_ABS" ]]; then
    echo "[error] initramfs image missing for staging verification: $INITRAMFS_IMAGE_ABS"
    return 1
  fi
  if ! command -v cpio >/dev/null 2>&1; then
    echo "[warn] cpio not found; skipping initramfs staged-path verification"
    return 0
  fi

  local listing
  if ! listing="$(cpio -it < "$INITRAMFS_IMAGE_ABS" 2>/dev/null)"; then
    echo "[error] failed to list initramfs image with cpio: $INITRAMFS_IMAGE_ABS"
    return 1
  fi

  local missing=0
  for expected in "init" "sbin/init_server" "sbin/initramfs_srv"; do
    if ! printf '%s\n' "$listing" | rg -q "^${expected}$"; then
      echo "[error] initramfs staged path missing: /${expected}"
      missing=1
    fi
  done
  if [[ "$missing" -ne 0 ]]; then
    return 1
  fi

  echo "[ok] initramfs staged path verified: /init"
  echo "[ok] initramfs staged path verified: /sbin/init_server"
  echo "[ok] initramfs staged path verified: /sbin/initramfs_srv"
  echo "[info] /init identity remains ${SERVER_BIN:-init_server}"
  echo "[info] /sbin/initramfs_srv identity is ${INITRAMFS_SERVER_BIN:-initramfs_srv}"
}
