#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP2-ACCEPTANCE §2 — the AArch64 invalidation sequence, checked in the BUILT kernel.
#
# Usage: scripts/check-aarch64-tlbi-sequence.sh <yarm-aarch64.elf>
#
# Every runtime translation invalidation is an inner-shareable broadcast TLBI. Architecturally
# (Arm ARM, break-before-make and TLB maintenance) it needs, in this order:
#   dsb ishst            the table write that precedes it (the break's empty leaf, or the make's
#                        new leaf) is visible to every table walker before the invalidation;
#   tlbi <op>is          the broadcast invalidation;
#   dsb ish              completes the invalidation on every PE of the inner-shareable domain —
#                        the completion the next make, and any reclaim, depends on;
#   isb                  the requester's own later instructions see it.
# QEMU TCG does not distinguish a missing barrier (it completes a broadcast TLBI synchronously),
# so this check is what rejects its omission; no boot can.
#
# Checked: every broadcast TLBI (`tlbi *is`) in the image is exactly that four-instruction
# sequence, and `page_table::unmap_page` (the break) and `page_table::map_page` (the make) each
# contain a `tlbi vaae1is` in it. Local, non-broadcast TLBIs (`tlbi vmalle1`) occur only in the
# MMU bring-up paths and are listed, not graded.
set -uo pipefail
ELF=${1:?usage: $0 <yarm-aarch64.elf>}
OBJDUMP=${OBJDUMP:-llvm-objdump}
"$OBJDUMP" -d --no-show-raw-insn "$ELF" > /dev/null 2>&1 || { echo "TLBI_SEQUENCE result=fail reason=disassembly"; exit 1; }
"$OBJDUMP" -d --no-show-raw-insn "$ELF" | python3 -c '
import re, sys
func, insns = None, []
for line in sys.stdin:
    m = re.match(r"^[0-9a-f]+ <(.+)>:$", line.strip())
    if m:
        func = m.group(1); continue
    m = re.match(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$", line.rstrip())
    if m and func:
        insns.append((func, int(m.group(1), 16), m.group(2), m.group(3).split("//")[0].strip()))
fails, broadcast, local, owners = [], 0, [], {"unmap_page": 0, "map_page": 0}
for i, (f, addr, op, args) in enumerate(insns):
    if op != "tlbi":
        continue
    kind = args.split(",")[0].strip()
    if not kind.endswith("is"):
        local.append("%s@0x%x (%s)" % (kind, addr, re.sub(r"^_R\w*?(\d+)(\w+?)$", r"\2", f)[:60]))
        continue
    broadcast += 1
    seq = [(o, a) for (_, _, o, a) in insns[i - 1:i + 3]]
    want = [("dsb", "ishst"), ("tlbi", args), ("dsb", "ish"), ("isb", "")]
    if seq != want or any(x[0] != f for x in insns[i - 1:i + 3]):
        fails.append("0x%x in %s: %s, want dsb ishst; tlbi %s; dsb ish; isb" % (addr, f[:80], "; ".join((o + " " + a).strip() for o, a in seq), kind))
        continue
    for name in owners:
        if kind == "vaae1is" and re.search(r"10page_table\d*%s$" % {"unmap_page": "10unmap_page", "map_page": "8map_page"}[name], f):
            owners[name] += 1
for name, n in owners.items():
    if n == 0:
        fails.append("page_table::%s contains no complete broadcast tlbi vaae1is sequence" % name)
for f in fails:
    print("[tlbi-sequence][fail] " + f)
print("[tlbi-sequence] broadcast=%d unmap_page=%d map_page=%d local=%d: %s" % (broadcast, owners["unmap_page"], owners["map_page"], len(local), ", ".join(local)))
print("TLBI_SEQUENCE broadcast=%d result=%s" % (broadcast, "fail" if fails or broadcast == 0 else "ok"))
sys.exit(1 if fails or broadcast == 0 else 0)
'
