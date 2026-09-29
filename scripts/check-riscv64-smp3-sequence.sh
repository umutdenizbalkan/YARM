#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3 — the RISC-V remote-fence and IPI ordering, checked in the BUILT image.
#
# Usage: scripts/check-riscv64-smp3-sequence.sh <kernel ELF>
#
# QEMU's TCG runs every hart's memory accesses in host order and flushes a whole TLB on any
# SFENCE.VMA, so a boot cannot tell whether these instructions are present. Each is required by
# the ordering argument in `arch::riscv64::ipi`, so the image itself is checked:
#
#   remote_invalidate_page  `fence rw, rw` BEFORE the SBI RFENCE `ecall` — the requester's PTE store
#                           is globally visible before any target is asked to SFENCE.VMA;
#   send_reschedule         the mailbox `amoor` (publication), then `fence rw, rw`, then the SBI IPI
#                           `ecall` (notification);
#   take_arrival            `csrc sip` (clear SSIP) BEFORE the mailbox `amoswap` (consume).
#
# Each owner is `#[inline(never)]`, so it is one named function in the image. Exits non-zero, and
# names the step, when any required instruction is missing or out of order.
set -uo pipefail
ELF=${1:?usage: $0 <kernel ELF>}
DIS=$(mktemp) || exit 2
trap 'rm -f "$DIS"' EXIT
llvm-objdump -d --no-show-raw-insn --demangle --mattr=+m,+a,+c "$ELF" >"$DIS" 2>/dev/null || { echo "SEQUENCE result=fail reason=objdump"; exit 2; }
python3 - "$DIS" <<'PY'
import re, sys
dis = open(sys.argv[1]).read().split("\n")
def body(name):
    out, on = [], False
    for l in dis:
        m = re.match(r"^[0-9a-f]+ <(.*)>:$", l)
        # A local label (`.Lpcrel_hi…`) sits INSIDE a function; only a symbol starts a new one.
        if m and not m.group(1).startswith(".L"):
            on = m.group(1).endswith("::" + name) and "ipi" in m.group(1)
            continue
        if on and l.strip():
            if not re.match(r"^[0-9a-f]+ <", l):
                out.append(re.sub(r"^\s*[0-9a-f]+:\s*", "", l).strip())
    return out
fails = []
def order(fn, steps):
    b = body(fn)
    if not b:
        fails.append("%s: not found in the image" % fn); return
    if any("<unknown>" in x for x in b):
        fails.append("%s: undecoded instructions in the body" % fn); return
    at = -1
    for label, pat in steps:
        hit = next((i for i in range(at + 1, len(b)) if re.search(pat, b[i])), None)
        if hit is None:
            fails.append("%s: %s missing (or not after the previous step)" % (fn, label)); return
        at = hit
        print("SEQUENCE %s %s at +%d: %s" % (fn, label, hit, b[hit]))
    # The ordered step (the last one) may not ALSO appear before the steps that must precede it.
    first = next((i for i, l in enumerate(b) if re.search(steps[-1][1], l)), None)
    if first is not None and first < at:
        fails.append("%s: a %s at +%d precedes the required order" % (fn, steps[-1][0], first))
order("remote_invalidate_page", [("fence", r"^fence\s+rw,\s*rw$"), ("rfence_ecall", r"^ecall$")])
order("send_reschedule", [("publication", r"^amoor\.d(\.aqrl|\.aq|\.rl)?\s"), ("fence", r"^fence\s+rw,\s*rw$"), ("ipi_ecall", r"^ecall$")])
order("take_arrival", [("clear_ssip", r"^csrc\s+sip,|^csrci\s+sip,|^csrrc\s+\w+,\s*sip,"), ("consume", r"^amoswap\.d(\.aqrl|\.aq|\.rl)?\s")])
for f in fails:
    print("SEQUENCE [fail] " + f)
print("SEQUENCE result=%s" % ("fail" if fails else "ok"))
sys.exit(1 if fails else 0)
PY
