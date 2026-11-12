#!/usr/bin/env python3
"""RISCV-PARK-RET1 — does any RISC-V user binary READ a7 after an ecall before writing it?
For each `ecall`, follow the straight-line instructions after it (stopping at a branch, jump, call
or return, where the analysis cannot follow) and report a read of a7 (as a source operand) that
precedes any write of a7. Usage: rpr_a7scan.py <objdump -d output>..."""
import re, sys

BRANCH = re.compile(r"^(j|jr|jal|jalr|ret|beq|bne|blt|bge|bltu|bgeu|beqz|bnez|blez|bgez|bltz|bgtz|"
                    r"c\.j|c\.jr|c\.jal|c\.jalr|c\.beqz|c\.bnez|tail|call|ecall|ebreak)$")
total = 0
for path in sys.argv[1:]:
    lines = [l for l in open(path) if re.match(r"^\s+[0-9a-f]+:", l)]
    ins = []
    for l in lines:
        parts = l.split("\t")
        # llvm-objdump: "  addr: bytes \t mnemonic \t operands"
        m = re.match(r"^\s+([0-9a-f]+):\s+(.*)$", l)
        toks = [t.strip() for t in l.split("\t") if t.strip()]
        if len(toks) < 2:
            continue
        mnem = toks[1] if len(toks) > 1 else ""
        ops = toks[2] if len(toks) > 2 else ""
        ins.append((m.group(1), mnem, ops))
    hits = ecalls = 0
    for i, (addr, mnem, ops) in enumerate(ins):
        if mnem != "ecall":
            continue
        ecalls += 1
        for a2, m2, o2 in ins[i + 1:i + 40]:
            regs = [r.strip() for r in re.split(r"[,()]", o2) if r.strip()]
            if BRANCH.match(m2):
                if "a7" in regs:
                    print(f"{path}: ecall@{addr} -> {a2} {m2} {o2}  (reads a7)")
                    hits += 1
                break
            if not regs:
                continue
            dst, srcs = regs[0], regs[1:]
            if m2.startswith(("s", "c.s")) and m2 not in ("sub", "subw", "sll", "slli", "srl", "srli",
                                                         "sra", "srai", "slt", "sltu", "slti", "sltiu",
                                                         "sext.w", "seqz", "snez", "sllw", "srlw", "sraw",
                                                         "slliw", "srliw", "sraiw", "c.slli", "c.sub",
                                                         "c.subw", "c.srli", "c.srai"):
                srcs = regs  # a store reads all its operands
                dst = None
            if "a7" in srcs:
                print(f"{path}: ecall@{addr} -> {a2} {m2} {o2}  (reads a7)")
                hits += 1
                break
            if dst == "a7":
                break
    total += hits
    print(f"{path}: ecalls={ecalls} a7_read_after_ecall={hits}")
print(f"TOTAL a7_read_after_ecall={total}")
