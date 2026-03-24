#!/usr/bin/env bash
set -euo pipefail

OUT_DIR=${OUT_DIR:-build-x86_64}
OUT_IMAGE=${OUT_IMAGE:-$OUT_DIR/linux-bzImage}
BZIMAGE_URL=${BZIMAGE_URL:-}
HTTP_USER_AGENT=${HTTP_USER_AGENT:-Mozilla/5.0 (X11; Linux x86_64) YARM/1.0}

mkdir -p "$(dirname "$OUT_IMAGE")"

if ! command -v curl >/dev/null 2>&1; then
  echo "[error] curl is required to download a bootable kernel image"
  exit 1
fi

download_one() {
  local url="$1"
  echo "[info] trying source: $url"
  curl -A "$HTTP_USER_AGENT" -fL "$url" -o "$OUT_IMAGE"
}

if [[ -n "$BZIMAGE_URL" ]]; then
  echo "[info] downloading Linux kernel image from explicit BZIMAGE_URL"
  echo "[info] source: $BZIMAGE_URL"
  echo "[info] dest:   $OUT_IMAGE"
  download_one "$BZIMAGE_URL"
else
  CANDIDATES=(
    "https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64/netboot/vmlinuz-lts"
    "https://mirrors.edge.kernel.org/alpine/latest-stable/releases/x86_64/netboot/vmlinuz-lts"
    "https://archive.ubuntu.com/ubuntu/dists/jammy-updates/main/installer-amd64/current/legacy-images/netboot/ubuntu-installer/amd64/linux"
  )
  echo "[info] downloading Linux kernel image"
  echo "[info] dest: $OUT_IMAGE"
  DOWNLOADED=0
  for url in "${CANDIDATES[@]}"; do
    if download_one "$url"; then
      DOWNLOADED=1
      break
    fi
    echo "[warn] failed: $url"
  done
  if [[ "$DOWNLOADED" -ne 1 ]]; then
    echo "[error] could not download a known bootable kernel image candidate"
    echo "[hint] provide BZIMAGE_URL=<url> and rerun this script"
    exit 1
  fi
fi

if command -v file >/dev/null 2>&1; then
  KERNEL_TYPE=$(file -b "$OUT_IMAGE" || true)
  echo "[info] file type: $KERNEL_TYPE"
  if [[ "$KERNEL_TYPE" != *"Linux kernel x86 boot executable bzImage"* ]]; then
    echo "[warn] downloaded file does not look like a bzImage according to 'file'"
    echo "[hint] you can still try it, or provide a different BZIMAGE_URL"
  fi
fi

echo "[ok] bootable image candidate downloaded: $OUT_IMAGE"
echo "[next] export KERNEL_BOOTABLE_IMAGE_SOURCE=$OUT_IMAGE"
echo "[next] run scripts/build-qemu-x86_64-artifacts.sh && scripts/qemu-x86_64-core-smoke.sh"
