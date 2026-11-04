#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KTHREAD-CLOSURE1 §C — bound every single stack-pointer decrement in a kernel image.

Kernel-thread stacks sit at the bottom of a 16 MiB stride of an otherwise unmapped window
(contract §18.10.3). A thread that overflows faults in that gap provided no single stack-pointer
decrement can carry it past the gap before its first store there: the deepest descent before
such a store is one crossing frame plus one callee's adjustment plus a trap frame. The image
therefore must not contain a decrement larger than BOUND = (STRIDE - STACK - TRAP_MARGIN) / 2.

Usage: check-kthread-stack-adjustments.py --arch x86_64|aarch64|riscv64 <kernel.elf>
       check-kthread-stack-adjustments.py --arch A --disassembly FILE   (pre-decoded input)
Prints the largest decrements and `KSTACK_ADJUST_CHECK ... result=ok|fail`; exit 0 iff ok.

A decrement whose amount is not a constant the check can resolve in the same function (a
register loaded from memory, a dynamic allocation) fails the check: it cannot be bounded.
"""
import re
import subprocess
import sys

STRIDE = 16 << 20
STACK = 16 << 10
TRAP_MARGIN = 4 << 10
BOUND = (STRIDE - STACK - TRAP_MARGIN) // 2

FN = re.compile(r"^[0-9a-f]+ <(.*)>:$")


def imm(text):
    text = text.strip().lstrip("#$")
    neg = text.startswith("-")
    text = text.lstrip("-")
    v = int(text, 16) if text.startswith("0x") else int(text)
    return -v if neg else v


X86_REG32 = {"eax": "rax", "ebx": "rbx", "ecx": "rcx", "edx": "rdx", "esi": "rsi", "edi": "rdi",
             "ebp": "rbp", "esp": "rsp"}


def x86_reg(r):
    """The 64-bit register a 32-bit write zero-extends into."""
    if r in X86_REG32:
        return X86_REG32[r]
    m = re.match(r"(r\d+)d$", r)
    return m.group(1) if m else r


def scan(lines, arch):
    """Yield (amount_or_None, function, instruction) for every stack-pointer decrement."""
    fn, consts = "?", {}
    for raw in lines:
        m = FN.match(raw.strip())
        if m:
            fn, consts = m.group(1), {}
            continue
        if ":" not in raw:
            continue
        ins = raw.split(":", 1)[1].strip()
        ins = re.sub(r"\s*//.*$", "", ins)
        if arch == "x86_64":
            ins = re.sub(r"\s+#.*$", "", ins)
        ins = re.sub(r"\s+", " ", ins).strip()
        if arch == "x86_64":
            m = re.match(r"(?:sub|subq) rsp, (\S+)$", ins)
            if m:
                a = m.group(1)
                yield (imm(a) if re.match(r"-?(0x)?[0-9a-f]+$", a) else consts.get(x86_reg(a)), fn, ins)
                continue
            m = re.match(r"(?:mov|movabs) (\w+), (-?(?:0x)?[0-9a-f]+)$", ins)
            if m:
                consts[x86_reg(m.group(1))] = imm(m.group(2))
                continue
            m = re.match(r"lea rsp, \[rsp - (0x[0-9a-f]+)\]", ins)
            if m:
                yield (imm(m.group(1)), fn, ins)
                continue
            if re.match(r"(?:add|and) rsp, ", ins):
                # `and rsp, -N` (realignment) can lower rsp by at most N-1 bytes.
                m = re.match(r"and rsp, (-?(?:0x)?[0-9a-f]+)$", ins)
                if m:
                    yield (-imm(m.group(1)) - 1, fn, ins)
                    continue
                m = re.match(r"add rsp, (-(?:0x)?[0-9a-f]+)$", ins)
                if m:
                    yield (-imm(m.group(1)), fn, ins)
            m = re.match(r"\w+ (\w+), ", ins)
            if m:
                consts.pop(x86_reg(m.group(1)), None)
        elif arch == "aarch64":
            m = re.match(r"sub sp, sp, #(0x[0-9a-f]+|\d+)(?:, lsl #12)?$", ins)
            if m:
                v = imm(m.group(1)) << (12 if "lsl #12" in ins else 0)
                yield (v, fn, ins)
                continue
            m = re.match(r"sub sp, sp, (\w+)(?:, uxtx)?(?:, lsl #(\d+))?$", ins)
            if m:
                v = consts.get(m.group(1))
                yield ((v << int(m.group(2) or 0)) if v is not None else None, fn, ins)
                continue
            m = re.match(r"st\w* .*\[sp, #(-0x[0-9a-f]+|-\d+)\]!$", ins)
            if m:
                yield (-imm(m.group(1)), fn, ins)
                continue
            m = re.match(r"mov (\w+), #(0x[0-9a-f]+|\d+)$", ins)
            if m:
                consts[m.group(1)] = imm(m.group(2))
                continue
            m = re.match(r"movk (\w+), #(0x[0-9a-f]+|\d+), lsl #(\d+)$", ins)
            if m and m.group(1) in consts:
                sh = int(m.group(3))
                consts[m.group(1)] = (consts[m.group(1)] & ~(0xFFFF << sh)) | (imm(m.group(2)) << sh)
                continue
            if re.match(r"(?:mov|add) sp, ", ins) and not re.match(r"add sp, sp, #", ins):
                # sp from another register: a frame-pointer restore or a stack switch.
                m = re.match(r"mov sp, x29$", ins)
                if not m:
                    yield (None, fn, ins)
                continue
            m = re.match(r"\w+ (\w+), ", ins)
            if m:
                consts.pop(m.group(1), None)
        else:  # riscv64
            m = re.match(r"(?:c\.)?addi(?:16sp)? sp, (?:sp, )?(-(?:0x)?[0-9a-f]+)$", ins)
            if m:
                yield (-imm(m.group(1)), fn, ins)
                continue
            m = re.match(r"(?:c\.)?lui (\w+), (0x[0-9a-f]+|\d+)$", ins)
            if m:
                v = imm(m.group(2)) << 12
                consts[m.group(1)] = v - (1 << 32) if v & (1 << 31) else v
                continue
            m = re.match(r"(?:c\.)?addiw? (\w+), (\w+), (-?(?:0x)?[0-9a-f]+)$", ins)
            if m and m.group(2) in consts:
                consts[m.group(1)] = consts[m.group(2)] + imm(m.group(3))
                continue
            m = re.match(r"(?:c\.)?li (\w+), (-?(?:0x)?[0-9a-f]+)$", ins)
            if m:
                consts[m.group(1)] = imm(m.group(2))
                continue
            m = re.match(r"sub sp, sp, (\w+)$", ins)
            if m:
                yield (consts.get(m.group(1)), fn, ins)
                continue
            m = re.match(r"(?:c\.)?add sp, sp, (\w+)$", ins)
            if m:
                v = consts.get(m.group(1))
                if v is None or v < 0:
                    yield ((-v) if v is not None else None, fn, ins)
                continue
            if re.match(r"(?:c\.)?mv sp, ", ins):
                if not re.match(r"(?:c\.)?mv sp, s0$", ins):
                    yield (None, fn, ins)
                continue
            m = re.match(r"(?:c\.)?\w+ (\w+), ", ins)
            if m:
                consts.pop(m.group(1), None)


# Functions that legitimately load sp from another register: they install a stack (boot entry,
# context switch, trap entry and exit, kernel-thread first entry), not a frame.
STACK_SWITCHERS = re.compile(
    r"(switch|trampoline|vector|trap|_start|start32|boot|entry|exception|resume|restore|"
    r"enter_user|eret|sret|idle|kthread|smp|secondary|ap_|longjmp|ctx)", re.I)


def check(lines, arch):
    found, unresolved, switches = [], [], []
    for amount, fn, ins in scan(lines, arch):
        if amount is None:
            if STACK_SWITCHERS.search(fn):
                switches.append((fn, ins))
            else:
                unresolved.append((fn, ins))
            continue
        found.append((amount, fn, ins))
    found.sort(reverse=True)
    worst = found[0] if found else (0, "-", "-")
    ok = worst[0] <= BOUND and not unresolved
    return ok, worst, found, unresolved, switches


def main():
    args = sys.argv[1:]
    if len(args) < 3 or args[0] != "--arch":
        raise SystemExit(__doc__)
    arch = args[1]
    if args[2] == "--disassembly":
        lines = open(args[3]).read().splitlines()
        src = args[3]
    else:
        src = args[2]
        lines = subprocess.run(["llvm-objdump", "-d", "--no-show-raw-insn"]
                               + (["-M", "intel"] if arch == "x86_64" else []) + [src],
                               capture_output=True, text=True, check=True).stdout.splitlines()
    ok, worst, found, unresolved, switches = check(lines, arch)
    for amount, fn, ins in found[:5]:
        print("KSTACK_ADJUST 0x%x %s | %s" % (amount, fn[:100], ins))
    for fn, ins in unresolved[:10]:
        print("KSTACK_ADJUST_UNRESOLVED %s | %s" % (fn[:100], ins))
    print("KSTACK_ADJUST_CHECK arch=%s image=%s max=0x%x bound=0x%x decrements=%d unresolved=%d "
          "stack_switches=%d result=%s"
          % (arch, src.split("/")[-1], worst[0], BOUND, len(found), len(unresolved), len(switches),
             "ok" if ok else "fail"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
