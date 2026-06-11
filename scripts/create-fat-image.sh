#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Stage 93: Create a small FAT12/FAT16 block image for YARM fat-block profile testing.
#
# Creates a 1 MiB FAT image containing:
#   hello.txt           — "Hello from YARM FAT!\n"
#   dir/nested.txt      — "Nested file in subdirectory.\n"
#
# Usage:
#   scripts/create-fat-image.sh [OUTPUT_IMAGE]
#
# Environment:
#   OUTPUT_IMAGE  — path to write the FAT image (default: build-fat/fat.img)
#   FAT_SIZE_MB   — image size in MiB (default: 1)
#
# Requirements:
#   mformat, mcopy (from mtools package)  — OR  mkfs.fat + mount (requires root)
#
# The image is usable as a QEMU virtio-blk backend:
#   -drive file=<image>,if=none,id=blk0,format=raw
#   -device virtio-blk-pci,drive=blk0

set -euo pipefail

OUTPUT_IMAGE=${1:-${OUTPUT_IMAGE:-build-fat/fat.img}}
FAT_SIZE_MB=${FAT_SIZE_MB:-1}

mkdir -p "$(dirname "$OUTPUT_IMAGE")"

echo "[info] create-fat-image: creating ${FAT_SIZE_MB} MiB FAT image at ${OUTPUT_IMAGE}"

if command -v mformat >/dev/null 2>&1 && command -v mcopy >/dev/null 2>&1; then
  # Preferred: use mtools (no root required)
  dd if=/dev/zero of="$OUTPUT_IMAGE" bs=1M count="$FAT_SIZE_MB" 2>/dev/null
  mformat -i "$OUTPUT_IMAGE" -F ::
  echo -n "Hello from YARM FAT!" | mcopy -i "$OUTPUT_IMAGE" - "::hello.txt"
  mmd -i "$OUTPUT_IMAGE" "::dir"
  echo -n "Nested file in subdirectory." | mcopy -i "$OUTPUT_IMAGE" - "::dir/nested.txt"
  echo "[ok] create-fat-image: FAT image created with mtools"
elif command -v mkfs.fat >/dev/null 2>&1; then
  # Fallback: mkfs.fat + loopback mount (requires root or user-namespace mount)
  dd if=/dev/zero of="$OUTPUT_IMAGE" bs=1M count="$FAT_SIZE_MB" 2>/dev/null
  mkfs.fat -F 12 "$OUTPUT_IMAGE" 2>/dev/null
  TMPDIR=$(mktemp -d)
  if mount -t vfat -o loop "$OUTPUT_IMAGE" "$TMPDIR" 2>/dev/null; then
    echo -n "Hello from YARM FAT!" > "$TMPDIR/hello.txt"
    mkdir -p "$TMPDIR/dir"
    echo -n "Nested file in subdirectory." > "$TMPDIR/dir/nested.txt"
    sync
    umount "$TMPDIR"
    rmdir "$TMPDIR"
    echo "[ok] create-fat-image: FAT image created with mkfs.fat + mount"
  else
    rmdir "$TMPDIR"
    echo "[warn] create-fat-image: mount failed (no root?); image has no files — for testing only"
    echo "[hint] install mtools (apt install mtools) for rootless FAT image creation"
  fi
else
  echo "[error] create-fat-image: neither mtools (mformat/mcopy) nor mkfs.fat found"
  echo "[hint] install mtools: apt install mtools"
  exit 1
fi

echo "[info] create-fat-image: image ready at ${OUTPUT_IMAGE} ($(wc -c < "$OUTPUT_IMAGE") bytes)"
