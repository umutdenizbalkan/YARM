#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 197A-C — INIT FAIL-FAST NEGATIVE TESTS (x86_64).
#
# The synthetic/placeholder init ELF fallback was removed in Stage 197A: a missing initramfs,
# a malformed CPIO, a missing `/init`, a malformed init ELF, or a forced ZC-load fault MUST halt
# boot with an explicit `BOOT_FATAL_*` diagnostic — never silently boot a fake init or limp on.
#
# This harness boots the REAL kernel ELF against deliberately-broken (or absent) initramfs images
# and a fault-injection knob, and asserts each fatal marker appears and that no post-fatal success
# (init/PM/service startup) occurs. Exits non-zero on any missing fatal marker or forbidden success.
set -uo pipefail
cd "$(dirname "$0")/.."

KERNEL_IMAGE=${KERNEL_IMAGE:-build-x86_64/kernel_boot.elf}
GOOD_INITRAMFS=${INITRAMFS_IMAGE:-build-x86_64/initramfs-core.cpio}
LOGDIR=${LOGDIR:-/tmp/init-fail-fast-neg}
TIMEOUT_SECS=${TIMEOUT_SECS:-40}
mkdir -p "$LOGDIR"
fail=0
note() { echo "[neg] $*"; }
die()  { echo "[neg][fail] $*"; fail=1; }

if [[ ! -f "$KERNEL_IMAGE" ]]; then
  echo "[neg][fail] missing kernel image: $KERNEL_IMAGE (build fresh first)"; exit 1
fi
if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
  echo "[neg][warn] qemu-system-x86_64 not installed; skipping"; exit 0
fi

# newc-format CPIO writer: pack_cpio <out> <name0> <file0> [<name1> <file1> ...] (TRAILER added).
pack_cpio() {
  python3 - "$@" <<'PY'
import sys, os
out = sys.argv[1]
pairs = sys.argv[2:]
def entry(name, data):
    name_b = name.encode() + b"\x00"
    hdr = "070701"
    fields = [0,0o100755,0,0,1,0,len(data),0,0,0,0,len(name_b),0]
    h = hdr + "".join("%08X" % f for f in fields)
    b = h.encode() + name_b
    b += b"\x00" * ((-len(b)) % 4)
    b += data
    b += b"\x00" * ((-len(b)) % 4)
    return b
blob = b""
for i in range(0, len(pairs), 2):
    name = pairs[i]
    with open(pairs[i+1], "rb") as f:
        blob += entry(name, f.read())
# TRAILER
name_b = b"TRAILER!!!\x00"
fields = [0,0,0,0,1,0,0,0,0,0,0,len(name_b),0]
h = "070701" + "".join("%08X" % f for f in fields)
tb = h.encode() + name_b
tb += b"\x00" * ((-len(tb)) % 4)
blob += tb
with open(out, "wb") as f:
    f.write(blob)
PY
}

# run_case <label> <logfile> <expect_marker> [<initrd_or_-none>] [<extra_cmdline>]
run_case() {
  local label="$1" log="$2" expect="$3" initrd="${4:-$GOOD_INITRAMFS}" extra="${5:-}"
  local cmdline="console=ttyS0 rdinit=/init${extra:+ $extra}"
  local args=(qemu-system-x86_64 -machine q35 -cpu qemu64 -m 512M -smp 1
    -nographic -monitor none -serial stdio -no-reboot -no-shutdown
    -kernel "$KERNEL_IMAGE" -append "$cmdline")
  if [[ "$initrd" != "-none" ]]; then
    args+=(-initrd "$initrd")
  fi
  note "case=$label expect='$expect' initrd=$initrd cmdline='$cmdline'"
  timeout --foreground "${TIMEOUT_SECS}s" stdbuf -oL -eL "${args[@]}" >"$log" 2>&1 || true
  # Force text (boot logs contain \0). Require the fatal marker.
  if ! tr '\r\0' '\n\n' <"$log" | rg -a -n "$expect" >/dev/null 2>&1; then
    die "case=$label MISSING expected fatal marker: $expect"
    return 1
  fi
  # No post-fatal success: the required services must never start after a fatal init load.
  # (The early `YARM_BOOT_OK present_cpus=...` CPU-online marker precedes the fatal and is fine;
  # a service-startup marker after the fatal would mean boot did not actually halt.)
  if tr '\r\0' '\n\n' <"$log" | rg -a -n "INITRAMFS_SRV_ENTRY|PM_SPAWN_V5_CAP_DECODE|EXT4_SRV_READY" >/dev/null 2>&1; then
    die "case=$label booted a real service after the fatal init-load halt"
    return 1
  fi
  note "case=$label OK ($expect)"
  return 0
}

# ── Case A: forced ZC-load fault via the default-off fault-injection knob. ──
run_case forced_zc "$LOGDIR/a_forced_zc.log" \
  "BOOT_FATAL_INIT_ZC_LOAD_FAILED reason=fault_injection" \
  "$GOOD_INITRAMFS" "yarm.force_init_zc_load_fail=1"

# ── Case B: missing initramfs (no -initrd at all). ──
run_case missing_initramfs "$LOGDIR/b_missing.log" \
  "BOOT_FATAL_INITRAMFS_MISSING" "-none"
if [[ -f "$LOGDIR/b_missing.log" ]] \
   && ! tr '\r\0' '\n\n' <"$LOGDIR/b_missing.log" | rg -a "BOOT_FATAL_NO_CPIO" >/dev/null 2>&1; then
  die "case=missing_initramfs MISSING BOOT_FATAL_NO_CPIO"
fi

# ── Case C: CPIO present but without a `/init` entry. ──
echo "not an init" >"$LOGDIR/dummy.txt"
pack_cpio "$LOGDIR/no_init.cpio" "sbin/dummy" "$LOGDIR/dummy.txt"
run_case cpio_no_init "$LOGDIR/c_no_init.log" \
  "BOOT_FATAL_INIT_NOT_FOUND path=/init" "$LOGDIR/no_init.cpio"

# ── Case D: `/init` present but not a valid ELF (garbage bytes). ──
printf 'this is definitely not an ELF binary\x00\x01\x02' >"$LOGDIR/bad_init.bin"
pack_cpio "$LOGDIR/bad_init.cpio" "init" "$LOGDIR/bad_init.bin"
run_case invalid_init_elf "$LOGDIR/d_bad_elf.log" \
  "BOOT_FATAL_INIT_ELF_INVALID" "$LOGDIR/bad_init.cpio"

if (( fail )); then
  echo "INIT_FAIL_FAST_NEGATIVE arch=x86_64 result=fail"
  exit 1
fi
echo "INIT_FAIL_FAST_NEGATIVE arch=x86_64 cases=4 result=ok"
exit 0
