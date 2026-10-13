#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# VM-INVALIDATION1 — the activation and invalidation instructions the invalidation contract relies
# on, checked in the BUILT kernel image. QEMU TCG flushes whole TLBs and completes invalidations
# synchronously, so no boot can reject their omission; this check does.
#
# Usage: scripts/check-vm-invalidation-sequence.sh <x86_64|aarch64|riscv64> <kernel ELF>
#
#   x86_64   Every CR3 load (`mov cr3, r`) flushes the incoming PCID: the value written never has
#            bit 63 (the no-flush hint) set. Graded per function: a function that loads CR3 may not
#            compose bit 63 (`bts r, 0x3f`, or a 0x8000000000000000 immediate) anywhere. The per-page
#            invalidation (`invlpg`) is present in the image.
#   riscv64  Every `csrw satp` is followed IMMEDIATELY by `sfence.vma zero, zero`: activation
#            forgets every translation the previous root, or an earlier tenant of the same ASID,
#            left behind. The remote ordering (fence before the RFENCE ecall) is
#            check-riscv64-smp3-sequence.sh.
#   aarch64  Every broadcast TLBI is `dsb ishst; tlbi *is; dsb ish; isb`
#            (check-aarch64-tlbi-sequence.sh), and the image contains both the per-page broadcast
#            (`tlbi vaae1is`) and the whole-ASID broadcast (`tlbi aside1is`) the teardown uses.
#
# Exits non-zero and names the site when a required instruction is missing or misplaced.
set -uo pipefail
ARCH=${1:?usage: $0 <x86_64|aarch64|riscv64> <kernel ELF>}
ELF=${2:?usage: $0 <x86_64|aarch64|riscv64> <kernel ELF>}
OBJDUMP=${OBJDUMP:-llvm-objdump}
DIS=$(mktemp) || exit 2
trap 'rm -f "$DIS"' EXIT
case "$ARCH" in
  x86_64)  extra=(-M intel) ;;
  riscv64) extra=(--mattr=+m,+a,+c) ;;
  aarch64) extra=() ;;
  *) echo "VMI_SEQUENCE result=fail reason=unknown_arch"; exit 2 ;;
esac
"$OBJDUMP" -d --no-show-raw-insn --demangle "${extra[@]}" "$ELF" >"$DIS" 2>/dev/null \
  || { echo "VMI_SEQUENCE arch=$ARCH result=fail reason=objdump"; exit 2; }
if [[ "$ARCH" == aarch64 ]]; then
  "$(dirname "$0")/check-aarch64-tlbi-sequence.sh" "$ELF" || { echo "VMI_SEQUENCE arch=aarch64 result=fail reason=tlbi_sequence"; exit 1; }
fi
python3 -I - "$ARCH" "$DIS" <<'PY'
import re, sys
arch, path = sys.argv[1], sys.argv[2]
funcs, cur = {}, None
for l in open(path, errors="replace"):
    l = l.rstrip("\n")
    m = re.match(r"^[0-9a-f]+ <(.*)>:$", l)
    if m:
        if not m.group(1).startswith(".L"):
            cur = m.group(1); funcs.setdefault(cur, [])
        continue
    m = re.match(r"^\s*([0-9a-f]+):\s+(.*)$", l)
    if m and cur is not None:
        funcs[cur].append((int(m.group(1), 16), re.sub(r"\s+", " ", m.group(2).split("#")[0].split("//")[0]).strip()))
fails, sites = [], 0
short = lambda f: f[:90]
if arch == "x86_64":
    for f, body in funcs.items():
        loads = [(a, i) for a, i in body if re.match(r"^mov cr3, \w+$", i)]
        if not loads:
            continue
        sites += len(loads)
        bad = [(a, i) for a, i in body if re.match(r"^bts \w+, 0x3f$", i)
               or re.search(r"0x8000000000000000\b|-0x8000000000000000\b|9223372036854775808", i)]
        for a, i in bad:
            fails.append("0x%x in %s: '%s' composes the CR3 no-flush bit in a CR3-loading function" % (a, short(f), i))
        print("VMI_SEQUENCE x86_64 cr3_load %s loads=%d" % (short(f), len(loads)))
    if not any(re.match(r"^invlpg ", i) for b in funcs.values() for _, i in b):
        fails.append("no invlpg in the image")
elif arch == "riscv64":
    for f, body in funcs.items():
        for k, (a, i) in enumerate(body):
            if not re.match(r"^csrw satp, \w+$", i):
                continue
            sites += 1
            nxt = body[k + 1][1] if k + 1 < len(body) else "<end>"
            if not re.match(r"^sfence\.vma( zero, zero)?$", nxt):
                fails.append("0x%x in %s: csrw satp followed by '%s', want sfence.vma zero, zero" % (a, short(f), nxt))
            else:
                print("VMI_SEQUENCE riscv64 satp_load 0x%x %s" % (a, short(f)))
elif arch == "aarch64":
    for op in ("vaae1is", "aside1is"):
        hits = [(f, a) for f, b in funcs.items() for a, i in b if re.match(r"^tlbi %s," % op, i)]
        sites += len(hits)
        if not hits:
            fails.append("no tlbi %s in the image" % op)
        for f, a in hits:
            print("VMI_SEQUENCE aarch64 tlbi_%s 0x%x %s" % (op, a, short(f)))
if sites == 0:
    fails.append("no graded sites found")
for f in fails:
    print("VMI_SEQUENCE [fail] " + f)
print("VMI_SEQUENCE arch=%s sites=%d result=%s" % (arch, sites, "fail" if fails else "ok"))
sys.exit(1 if fails else 0)
PY
