#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KERNEL-NOFP1 — final-image check: ordinary kernel code uses no FP/SIMD.

    check-kernel-nofp.py --arch x86_64|aarch64|riscv64 --elf KERNEL_ELF
                         [--features LIST] [--toolchain TEXT] [--boot-image BIN]
    check-kernel-nofp.py --census ...      report populations only; never passes
    check-kernel-nofp.py --self-test       classifier and policy unit checks

What it inspects is the final linked ELF that boots (for AArch64, `--boot-image` additionally
proves the raw image QEMU loads is exactly this ELF's loadable bytes).

1. Every executable section is partitioned into regions. The default region is ordinary code in
   the port's native mode. The image's own `.yarm_nofp_map` table (a non-loaded section the
   sources emit next to the code they describe) can mark sub-ranges as 16-bit or 32-bit code,
   embedded data, or an embedded USER-MODE program the kernel copies out and never executes.
2. Every code region is disassembled IN FULL, in its mode, with zero blocks included. The decoded
   instructions must tile the region exactly; an undecodable byte, a gap, a missing disassembler
   or an unparseable line is a failure ("inspection incomplete"), never a pass. AArch64 literal
   pools are accounted as data through the assembler's own mapping symbols.
3. Each instruction is classified: FP/SIMD (an FP/SIMD/vector register operand, an x87/FP/vector
   mnemonic, or an implicit-state instruction such as FXSAVE/LDMXCSR/XSAVE/EMMS), FP control
   state (MXCSR, FCW, FPCR/FPSR, fcsr/frm/fflags, vector CSRs), or FP-enable control
   (CR0/CR4/XCR0 writes, CLTS, LMSW; CPACR_EL1/CPTR_ELx writes).
4. Every classified instruction must lie inside an audited EXCEPTION record of the map. Each
   exception kind names its purpose; the checker holds, per kind, the exact ordered list of
   FP/control instructions it may contain, a bound on its total size, and how many instances the
   build must carry. Anything else inside an exception range (an unrelated FP operation, a missing
   save, a reordered restore) fails it.

The result line binds the verdict to the image hash, the features, the toolchain and the
disassembler.
"""

import argparse
import hashlib
import os
import re
import shutil
import struct
import subprocess
import sys

MAGIC = 0x3150464F4E4D5259  # b"YRMNOFP1", little-endian
MAP_SECTION = ".yarm_nofp_map"

# ── ELF ─────────────────────────────────────────────────────────────────────────────────────────

SHF_ALLOC = 0x2
SHF_EXECINSTR = 0x4
SHT_NOBITS = 8
SHT_SYMTAB = 2
PT_LOAD = 1
PF_X = 1


class Elf:
    def __init__(self, path):
        with open(path, "rb") as f:
            self.data = f.read()
        d = self.data
        if d[:4] != b"\x7fELF" or d[4] != 2 or d[5] != 1:
            raise SystemExit(f"[nofp] {path}: not a little-endian ELF64 file")
        (self.e_machine,) = struct.unpack_from("<H", d, 18)
        e_phoff, e_shoff = struct.unpack_from("<QQ", d, 32)
        e_phentsize, e_phnum, e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHHHH", d, 54)
        self.sections = []
        for i in range(e_shnum):
            o = e_shoff + i * e_shentsize
            name, typ, flags, addr, off, size, link, info, align, entsize = struct.unpack_from(
                "<IIQQQQIIQQ", d, o)
            self.sections.append(dict(idx=i, name_off=name, type=typ, flags=flags, addr=addr,
                                      off=off, size=size, link=link, entsize=entsize))
        shstr = self.sections[e_shstrndx]
        for s in self.sections:
            s["name"] = self._cstr(shstr["off"] + s["name_off"])
        self.segments = []
        for i in range(e_phnum):
            o = e_phoff + i * e_phentsize
            typ, flags, off, vaddr, paddr, filesz, memsz, align = struct.unpack_from(
                "<IIQQQQQQ", d, o)
            self.segments.append(dict(type=typ, flags=flags, off=off, vaddr=vaddr, paddr=paddr,
                                      filesz=filesz, memsz=memsz))
        self.symbols = []
        for s in self.sections:
            if s["type"] != SHT_SYMTAB:
                continue
            strtab = self.sections[s["link"]]
            for j in range(s["size"] // 24):
                o = s["off"] + j * 24
                name, info, other, shndx, value, size = struct.unpack_from("<IBBHQQ", d, o)
                self.symbols.append(dict(name=self._cstr(strtab["off"] + name), value=value,
                                         size=size, type=info & 0xF, shndx=shndx))

    def _cstr(self, off):
        end = self.data.index(b"\0", off)
        return self.data[off:end].decode("latin-1")

    def section(self, name):
        for s in self.sections:
            if s["name"] == name:
                return s
        return None

    def bytes_at(self, addr, size):
        for s in self.sections:
            if s["type"] != SHT_NOBITS and s["addr"] <= addr and addr + size <= s["addr"] + s["size"]:
                o = s["off"] + addr - s["addr"]
                return self.data[o:o + size]
        return None


def parse_map(elf):
    """The image's own exception/region table. Returns (records, problems)."""
    s = elf.section(MAP_SECTION)
    if s is None:
        return [], []
    raw = elf.data[s["off"]:s["off"] + s["size"]]
    recs, problems, i = [], [], 0
    while i + 8 <= len(raw):
        (q,) = struct.unpack_from("<Q", raw, i)
        if q == 0:
            i += 8
            continue
        if q != MAGIC:
            problems.append(f"map: unrecognised word {q:#x} at offset {i:#x}")
            break
        if i + 24 > len(raw):
            problems.append(f"map: truncated record at offset {i:#x}")
            break
        start, end = struct.unpack_from("<QQ", raw, i + 8)
        j = raw.find(b"\0", i + 24)
        if j < 0:
            problems.append(f"map: unterminated name at offset {i:#x}")
            break
        name = raw[i + 24:j].decode("ascii", "replace")
        recs.append(dict(start=start, end=end, name=name))
        i = (j + 1 + 7) & ~7
    return recs, problems


# ── disassembly ─────────────────────────────────────────────────────────────────────────────────

RV_MATTR = "+m,+a,+f,+d,+c,+v,+zicsr,+zifencei,+zihintpause,+zfh,+zfhmin,+zba,+zbb,+zbs"
# `addr: <raw> <text>`; raw is byte pairs (GNU x86, LLVM RISC-V) or whole 16/32-bit words (LLVM
# AArch64, and RISC-V `.short`/`.word` data).
LINE = re.compile(r"^\s*([0-9a-f]+):\s+((?:(?:[0-9a-f]{2}|[0-9a-f]{4}|[0-9a-f]{8})\s)+)\s*(.*)$")


def tool(name):
    p = shutil.which(name)
    if not p:
        raise SystemExit(f"[nofp] inspection incomplete: required disassembler `{name}` not found")
    return p


def tool_version(name):
    out = subprocess.run([tool(name), "--version"], capture_output=True, text=True).stdout
    return out.strip().splitlines()[0] if out.strip() else name


def disassemble(arch, elf_path, section, lo, hi, mode):
    """Instructions of [lo, hi) as (addr, length, text, is_data)."""
    if arch == "x86_64":
        m = {"code64": "x86-64", "code32": "i386", "code16": "i8086"}[mode]
        cmd = [tool("objdump"), "-d", "-w", "-z", f"-M{m}", "-j", section,
               f"--start-address={lo:#x}", f"--stop-address={hi:#x}", elf_path]
    else:
        cmd = [tool("llvm-objdump"), "-d", "-z", "--no-print-imm-hex", f"--section={section}",
               f"--start-address={lo:#x}", f"--stop-address={hi:#x}", elf_path]
        if arch == "riscv64":
            cmd.insert(1, f"--mattr={RV_MATTR}")
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"[nofp] inspection incomplete: {' '.join(cmd[:3])} failed: {r.stderr[:300]}")
    out = []
    for line in r.stdout.splitlines():
        m = LINE.match(line)
        if not m:
            continue
        addr = int(m.group(1), 16)
        nbytes = sum(len(tok) // 2 for tok in m.group(2).split())
        text = m.group(3).strip()
        if arch == "x86_64" and not text and out:
            # GNU objdump continues a long instruction's bytes on the next line.
            a, n, t, dd = out[-1]
            out[-1] = (a, n + nbytes, t, dd)
            continue
        is_data = bool(re.match(r"^\.(word|short|byte|hword|inst)\b", text))
        out.append((addr, nbytes, text, is_data))
    return out


# ── classification ──────────────────────────────────────────────────────────────────────────────

def normalise(arch, text):
    t = text.split("//")[0] if arch != "x86_64" else text
    sym = None
    if arch == "x86_64":
        m = re.search(r"#\s*[0-9a-f]+\s*<([^>+]+)(\+0x[0-9a-f]+)?>", t)
        if m:
            sym = m.group(1)
        t = t.split("#")[0]
        if sym:
            t = re.sub(r"-?0x[0-9a-f]+\(%rip\)", f"{sym}(%rip)", t)
    t = re.sub(r"<[^>]*>", "", t)
    t = re.sub(r"\s+", " ", t).strip().lower()
    t = re.sub(r"\s*,\s*", ",", t)
    return t


def split(t):
    p = t.split(" ", 1)
    mnem = p[0]
    ops = p[1] if len(p) > 1 else ""
    # x86 prefixes printed as separate words
    while mnem in ("rex", "rex.w", "rex.b", "rex.r", "rex.x", "data16", "addr32", "lock", "rep",
                   "repz", "repnz", "notrack", "bnd", "cs", "ds", "ss", "es", "fs", "gs") and ops:
        q = ops.split(" ", 1)
        mnem, ops = q[0], (q[1] if len(q) > 1 else "")
    return mnem, ops


X86_FP_REG = re.compile(r"%(?:[xyz]mm\d+|mm[0-7]|st(?:\(\d\))?|k[0-7])\b")
X86_FP_IMPLICIT = {"emms", "femms", "vzeroupper", "vzeroall", "ldmxcsr", "stmxcsr", "vldmxcsr", "vstmxcsr", "wait",
                   "xsave", "xsave64", "xsaveopt", "xsaveopt64", "xsavec", "xsavec64", "xsaves",
                   "xsaves64", "xrstor", "xrstor64", "xrstors", "xrstors64"}
X86_CTRL = re.compile(r"^mov[lq]?$")

A64_FP_REG = re.compile(r"(?<![\w.#])(?:[qdshb](?:[12]?\d|3[01])|v(?:[12]?\d|3[01])(?:\.\d*[bhsdq])?|"
                        r"z(?:[12]?\d|3[01])|p(?:1[0-5]|\d)|za)(?![\w])")
A64_FP_SYSREG = re.compile(r"\b(fpcr|fpsr|fpmr|zcr_el[123]|smcr_el[123]|svcr|tpidr2_el0)\b")
A64_FP_ENABLE = re.compile(r"\b(cpacr_el1|cpacr_el12|cptr_el2|cptr_el3)\b")

RV_FP_REG = re.compile(r"(?<![\w.])(?:f(?:[12]?\d|3[01])|ft(?:1[01]|\d)|fs(?:1[01]|\d)|fa[0-7]|"
                       r"v(?:[12]?\d|3[01]))(?![\w])")
RV_FP_CSR = re.compile(r"\b(fflags|frm|fcsr|vstart|vxsat|vxrm|vcsr|vl|vtype|vlenb)\b")


def classify(arch, norm):
    """'fp' (FP/SIMD/vector use or FP control state), 'enable' (FP-enable control write), or None."""
    mnem, ops = split(norm)
    if arch == "x86_64":
        if X86_FP_REG.search(ops):
            return "fp"
        if mnem.startswith("f") or mnem in X86_FP_IMPLICIT:
            return "fp"
        if mnem in ("clts", "lmsw", "xsetbv"):
            return "enable"
        if X86_CTRL.match(mnem) and re.search(r",%cr[04]$", ops):
            return "enable"
        return None
    if arch == "aarch64":
        if A64_FP_SYSREG.search(ops):
            return "fp"
        if mnem == "msr" and A64_FP_ENABLE.search(ops.split(",")[0]):
            return "enable"
        if mnem in ("mrs", "msr", "sys", "sysl"):
            return None  # other system registers are not FP state
        if A64_FP_REG.search(ops):
            return "fp"
        if mnem.startswith("f") or mnem.startswith("scvtf") or mnem.startswith("ucvtf"):
            return "fp"
        return None
    if arch == "riscv64":
        if mnem.startswith("csr") or mnem.startswith("fr") or mnem.startswith("fs"):
            if RV_FP_CSR.search(ops) or mnem in ("frcsr", "fscsr", "frrm", "fsrm", "frflags",
                                                 "fsflags", "fsrmi", "fsflagsi"):
                return "fp"
        if mnem.startswith("f") and not mnem.startswith("fence"):
            return "fp"
        if mnem.startswith("v"):
            return "fp"
        if RV_FP_REG.search(ops):
            return "fp"
        return None
    raise SystemExit(f"unknown arch {arch}")


# ── policy: the audited exception inventory ────────────────────────────────────────────────────
#
# kind -> (purpose, ordered FP/control instruction patterns (full regex match on the normalised
# text), maximum total instructions in the range, expected-instance function of the feature set).

def always(n):
    return lambda feats: n


def if_feature(f, n=1):
    return lambda feats: n if f in feats else 0


GPR64 = r"%r(?:[a-d]x|[sd]i|bp|\d{1,2})"

POLICY = {
    "x86_64": {
        "X86_TRAP_ENTRY_CAPTURE": (
            "trap/syscall entry: capture the interrupted x87/SSE state before any kernel "
            "instruction, then install the kernel's own control environment",
            [r"fxsave64 \(%rsp\)", r"fninit", r"ldmxcsr yarm_x86_kernel_mxcsr\(%rip\)"], 4, always(2)),
        "X86_TRAP_ENTRY_RESTORE": (
            "trap/syscall return: restore the trap's captured x87/SSE state after the last kernel "
            "instruction", [r"fxrstor64 \(%rsp\)"], 1, always(2)),
        "X86_FIRST_ENTRY_RESTORE": (
            "first ring-3 entry: install the architectural initial x87/SSE image",
            [r"fxrstor64 yarm_x86_initial_fxsave\(%rip\)"], 1, always(1)),
        "X86_RESUME_RESTORE": (
            "saved-frame ring-3 resume: install the resumed task's own x87/SSE home",
            [r"fxrstor64 \(%rsi\)"], 1, always(1)),
        "X86_SWITCH_SAVE": (
            "kernel switch frame: save the outgoing kernel continuation's FXSAVE area",
            [r"fxsave 0x40\(%rdi\)"], 1, always(1)),
        "X86_SWITCH_RESTORE": (
            "kernel switch frame: restore the incoming kernel continuation's FXSAVE area",
            [r"fxrstor 0x40\(%rsi\)"], 1, always(1)),
        "X86_SWITCH_FRAME_INIT": (
            "new kernel switch frame: seed its FXSAVE area from the reset state",
            [r"fninit", rf"fxsave \({GPR64}\)"], 2, always(1)),
        "X86_KERNEL_FP_ENV_INIT": (
            "per-CPU bring-up: the kernel's own x87/MXCSR control environment",
            [r"fninit", r"ldmxcsr yarm_x86_kernel_mxcsr\(%rip\)"], 2, always(1)),
        "X86_CR0_WRITE": (
            "CR0 write (protected mode / paging / EM-TS-MP-NE for the eager user policy)",
            [rf"mov[lq]? %(?:e|r)ax,%cr0"], 1, always(4)),
        "X86_CR4_WRITE": (
            "CR4 write (PAE/PGE/SMEP/SMAP and OSFXSR/OSXMMEXCPT for the eager user policy)",
            [rf"mov[lq]? %(?:e|r)ax,%cr4"], 1, always(4)),
        "X86_CTX1_ENV_PROBE": (
            "CONTEXT1 witness: read the kernel's MXCSR and FCW on entry from ring 3",
            [rf"stmxcsr \({GPR64}\)", rf"fnstcw \({GPR64}\)"], 2, if_feature("context1-witness")),
        "X86_CTX1_CLOBBER": (
            "CONTEXT1 clobber witness: overwrite user-visible x87/SSE state with junk after "
            "dispatch (proves the trap path's save/restore)",
            [rf"fxrstor64 \({GPR64}\)"], 1, if_feature("context1-clobber")),
    },
    "aarch64": {
        "A64_VECTOR_CAPTURE": (
            "exception vector: capture q0..q31, FPCR and FPSR before the first call, then install "
            "the kernel's FPCR",
            [rf"stp q{i},q{i+1},\[sp,#{288 + 16 * i}\]" for i in range(0, 32, 2)]
            + [r"mrs x9,fpcr", r"mrs x9,fpsr", r"msr fpcr,xzr"], 32, always(1)),
        "A64_VECTOR_RESTORE": (
            "exception return: restore q0..q31, FPCR and FPSR after the last call",
            [rf"ldp q{i},q{i+1},\[sp,#{288 + 16 * i}\]" for i in range(0, 32, 2)]
            + [r"msr fpcr,x9", r"msr fpsr,x9"], 24, always(1)),
        "A64_FIRST_ENTRY_INIT": (
            "first EL0 entry: the architectural initial FP/SIMD state (all zero)",
            [rf"movi v{i}\.2d,#0(?:000000000000000)?" for i in range(32)]
            + [r"msr fpcr,xzr", r"msr fpsr,xzr"], 36, always(1)),
        "A64_SWITCH_SAVE": (
            "kernel switch frame: save callee-saved q8..q15",
            [rf"stp q{i},q{i+1},\[x0,#{128 + 16 * (i - 8)}\]" for i in range(8, 16, 2)], 4, always(1)),
        "A64_SWITCH_RESTORE": (
            "kernel switch frame: restore callee-saved q8..q15",
            [rf"ldp q{i},q{i+1},\[x1,#{128 + 16 * (i - 8)}\]" for i in range(8, 16, 2)], 4, always(1)),
        "A64_CPACR_WRITE": (
            "CPACR_EL1 write: FPEN=0b11 so the vector prologue may capture user FP/SIMD state",
            [r"msr cpacr_el1,x\d+"], 1, always(3)),
        "A64_CPTR_EL2_WRITE": (
            "CPTR_EL2 cleared at EL2 boot so EL1/EL0 FP/SIMD accesses are not trapped to EL2",
            [r"msr cptr_el2,xzr"], 1, always(1)),
        "A64_CTX1_ENV_PROBE": (
            "CONTEXT1 witness: read the kernel's FPCR on entry from EL0",
            [r"mrs x\d+,fpcr"], 1, if_feature("context1-witness")),
        "A64_CTX1_CLOBBER": (
            "CONTEXT1 clobber witness: overwrite q0..q31, FPCR and FPSR with junk after dispatch",
            [rf"ldp q{i},q{i+1},\[x\d+(?:,#{16 * i})?\]" if i == 0 else
             rf"ldp q{i},q{i+1},\[x\d+,#{16 * i}\]" for i in range(0, 32, 2)]
            + [r"msr fpcr,x\d+", r"msr fpsr,x\d+"], 24, if_feature("context1-clobber")),
    },
    "riscv64": {},
}

REGION_KINDS = {"CODE16": "code16", "CODE32": "code32", "DATA": None}
USER_PROGRAMS = {
    "x86_64": {"smp1_witness", "apsched_witness", "overtaken_witness", "vmi2_witness"},
    "aarch64": {"smp2_witness", "apsched_witness", "overtaken_witness"},
    "riscv64": {"smp3_witness", "apsched_witness", "sr_replace_witness", "vmi_witness"},
}
FEATURE_IMPLIES = {"context1-clobber": {"context1-witness"}}


def expand(features):
    feats = {f.strip() for f in features.replace(" ", ",").split(",") if f.strip()}
    changed = True
    while changed:
        changed = False
        for f in list(feats):
            for g in FEATURE_IMPLIES.get(f, ()):
                if g not in feats:
                    feats.add(g)
                    changed = True
    return feats


# ── the check ───────────────────────────────────────────────────────────────────────────────────

def symbolizer(elf):
    funcs = sorted((s["value"], s["name"]) for s in elf.symbols
                   if s["name"] and not s["name"].startswith("$") and s["type"] in (0, 1, 2))
    addrs = [a for a, _ in funcs]

    def at(addr):
        import bisect
        i = bisect.bisect_right(addrs, addr) - 1
        if i < 0:
            return f"{addr:#x}"
        return f"{funcs[i][1]}+{addr - funcs[i][0]:#x}"
    return at


def check(arch, elf_path, features, census=False, boot_image=None):
    elf = Elf(elf_path)
    feats = expand(features)
    sym = symbolizer(elf)
    recs, problems = parse_map(elf)
    fails = list(problems)
    report = []

    exec_secs = [s for s in elf.sections if s["flags"] & SHF_EXECINSTR and s["flags"] & SHF_ALLOC
                 and s["size"] > 0]
    if not exec_secs:
        fails.append("no executable sections")

    # Exec-segment coverage: every byte of an executable LOAD segment belongs to some section.
    for seg in elf.segments:
        if seg["type"] != PT_LOAD or not seg["flags"] & PF_X:
            continue
        lo, hi = seg["vaddr"], seg["vaddr"] + seg["memsz"]
        covered = sorted((max(lo, s["addr"]), min(hi, s["addr"] + s["size"])) for s in elf.sections
                         if s["flags"] & SHF_ALLOC and s["addr"] < hi and s["addr"] + s["size"] > lo)
        cur = lo
        for a, b in covered:
            if a > cur:
                gap = elf.bytes_at(cur, a - cur)
                if gap is not None and any(gap):
                    fails.append(f"executable segment {lo:#x}: unaccounted non-zero bytes {cur:#x}..{a:#x}")
            cur = max(cur, b)
        for s in elf.sections:
            if (s["flags"] & SHF_ALLOC and not s["flags"] & SHF_EXECINSTR and s["addr"] < hi
                    and s["addr"] + s["size"] > lo and s["size"]):
                report.append(f"data section in executable segment (not decoded, not executed as "
                              f"code by design): {s['name']} {s['size']} bytes")

    # Separate the map into regions and exceptions; validate each record's placement.
    live, eliminated = [], []
    for r in recs:
        if r["start"] == 0 and r["end"] == 0:
            eliminated.append(r)
            continue
        if not r["end"] > r["start"]:
            fails.append(f"map record {r['name']}: empty or inverted range {r['start']:#x}..{r['end']:#x}")
            continue
        sec = [s for s in exec_secs if s["addr"] <= r["start"] and r["end"] <= s["addr"] + s["size"]]
        if not sec:
            fails.append(f"map record {r['name']} {r['start']:#x}..{r['end']:#x}: not inside one executable section")
            continue
        r["section"] = sec[0]["name"]
        live.append(r)
    regions = []
    exceptions = []
    for r in live:
        head = r["name"].split(" ", 1)[0]
        if head in REGION_KINDS:
            regions.append(dict(r, mode=REGION_KINDS[head]))
        elif head == "USER_PROGRAM":
            prog = r["name"].split(" ", 1)[1] if " " in r["name"] else ""
            if prog not in USER_PROGRAMS[arch]:
                fails.append(f"map: unknown embedded user program `{prog}`")
            regions.append(dict(r, mode="user", program=prog))
        elif head in POLICY[arch]:
            exceptions.append(r)
        else:
            fails.append(f"map: unknown record kind `{r['name']}` at {r['start']:#x}")
    regions.sort(key=lambda r: r["start"])
    for a, b in zip(regions, regions[1:]):
        if b["start"] < a["end"]:
            fails.append(f"map: regions overlap: {a['name']} and {b['name']}")
    exceptions.sort(key=lambda r: r["start"])
    for a, b in zip(exceptions, exceptions[1:]):
        if b["start"] < a["end"]:
            fails.append(f"map: exception ranges overlap: {a['name']} and {b['name']}")

    # Decode every code byte in its mode.
    native = "code64" if arch == "x86_64" else "native"
    insns = []          # (addr, len, norm, cls, mode)
    acct = dict(code=0, data=0, user=0, sections=0)
    pops = {}
    for s in exec_secs:
        acct["sections"] += s["size"]
        lo, hi = s["addr"], s["addr"] + s["size"]
        cuts, cur = [], lo
        for r in [r for r in regions if r["section"] == s["name"]]:
            if r["start"] > cur:
                cuts.append((cur, r["start"], native))
            cuts.append((r["start"], r["end"], r["mode"]))
            cur = r["end"]
        if cur < hi:
            cuts.append((cur, hi, native))
        for a, b, mode in cuts:
            if mode is None:
                acct["data"] += b - a
                continue
            if mode == "user":
                acct["user"] += b - a
                continue
            dis = disassemble(arch, elf_path, s["name"], a, b, "code64" if mode == "native" else mode)
            pos = a
            for addr, n, text, is_data in dis:
                if addr != pos:
                    fails.append(f"inspection incomplete: {s['name']} {mode}: gap or overlap at "
                                 f"{pos:#x} (next decoded {addr:#x}) [{sym(pos)}]")
                    pos = addr
                pos = addr + n
                if is_data:
                    acct["data"] += n
                    continue
                if "<unknown>" in text or "(bad)" in text or not text:
                    fails.append(f"inspection incomplete: undecodable bytes in code at {addr:#x} "
                                 f"[{sym(addr)}] ({mode})")
                    continue
                acct["code"] += n
                norm = normalise(arch, text)
                cls = classify(arch, norm)
                if cls:
                    insns.append((addr, n, norm, cls, mode))
                    key = f"{cls}:{split(norm)[0]}"
                    pops[key] = pops.get(key, 0) + 1
            if pos != b:
                fails.append(f"inspection incomplete: {s['name']} {mode}: decoded to {pos:#x}, "
                             f"region ends {b:#x} [{sym(pos)}]")

    # Every classified instruction belongs to exactly one exception range.
    import bisect
    starts = [e["start"] for e in exceptions]
    inside = {id(e): [] for e in exceptions}
    ordinary = []
    for addr, n, norm, cls, mode in insns:
        i = bisect.bisect_right(starts, addr) - 1
        if i >= 0 and addr + n <= exceptions[i]["end"]:
            inside[id(exceptions[i])].append((addr, norm, cls))
        else:
            ordinary.append((addr, norm, cls))
    for addr, norm, cls in ordinary:
        fails.append(f"FORBIDDEN {cls} instruction in ordinary code at {addr:#x} [{sym(addr)}]: {norm}")

    # Each exception range: exactly its kind's ordered pattern, bounded size, instance counts.
    counts = {}
    for e in exceptions:
        kind = e["name"].split(" ", 1)[0]
        purpose, pats, max_insns, _ = POLICY[arch][kind]
        counts[kind] = counts.get(kind, 0) + 1
        got = inside[id(e)]
        where = f"{kind} at {e['start']:#x} [{sym(e['start'])}]"
        if len(got) != len(pats):
            fails.append(f"exception {where}: {len(got)} FP/control instructions, policy allows "
                         f"exactly {len(pats)}: {[g[1] for g in got]}")
        else:
            for (addr, norm, cls), pat in zip(got, pats):
                if not re.fullmatch(pat, norm):
                    fails.append(f"exception {where}: instruction at {addr:#x} `{norm}` is not the "
                                 f"permitted `{pat}`")
        if e["end"] - e["start"] > max(max_insns, len(pats)) * 16 + 128:
            fails.append(f"exception {where}: range of {e['end'] - e['start']} bytes exceeds its bound")
    for kind, (purpose, pats, mx, expected) in POLICY[arch].items():
        want = expected(feats)
        have = counts.get(kind, 0)
        if have != want:
            fails.append(f"exception inventory: {kind} has {have} instance(s), this build "
                         f"(features {sorted(feats) or ['<none>']}) requires {want}")

    if boot_image:
        fails += check_boot_image(elf, boot_image)

    return dict(fails=fails, report=report, acct=acct, pops=pops, exceptions=exceptions,
                eliminated=eliminated, regions=regions, ordinary=ordinary, insns=insns,
                feats=feats, sym=sym)


def check_boot_image(elf, path):
    """The raw image QEMU loads must be exactly this ELF's loadable bytes (objcopy -O binary)."""
    out = []
    with open(path, "rb") as f:
        img = f.read()
    loads = [s for s in elf.segments if s["type"] == PT_LOAD and s["filesz"]]
    if not loads:
        return ["boot image: ELF has no loadable bytes"]
    base = min(s["paddr"] for s in loads)
    for s in loads:
        o = s["paddr"] - base
        want = elf.data[s["off"]:s["off"] + s["filesz"]]
        if img[o:o + len(want)] != want:
            out.append(f"boot image {path}: bytes at {o:#x} differ from the ELF segment at {s['paddr']:#x}")
    return out


# ── self-test ───────────────────────────────────────────────────────────────────────────────────

SELF_TEST = [
    ("x86_64", "movaps %xmm0,0x10(%rsp)", "fp"), ("x86_64", "movdqu (%rsi),%xmm1", "fp"),
    ("x86_64", "pxor %xmm0,%xmm0", "fp"), ("x86_64", "fld1", "fp"), ("x86_64", "fninit", "fp"),
    ("x86_64", "fxsave64 (%rsp)", "fp"), ("x86_64", "ldmxcsr 0x10(%rip)", "fp"),
    ("x86_64", "emms", "fp"), ("x86_64", "xsave (%rdi)", "fp"), ("x86_64", "vzeroupper", "fp"),
    ("x86_64", "vmovdqa %ymm0,%ymm1", "fp"), ("x86_64", "movq %mm0,%rax", "fp"),
    ("x86_64", "mov %rax,%cr4", "enable"), ("x86_64", "mov %rax,%cr0", "enable"),
    ("x86_64", "mov %cr4,%rax", None), ("x86_64", "mov %rax,%cr3", None), ("x86_64", "clts", "enable"),
    ("x86_64", "xsetbv", "enable"), ("x86_64", "mov %rax,%rbx", None), ("x86_64", "lfence", None),
    ("x86_64", "movnti %rax,(%rdi)", None), ("x86_64", "rex.w fxrstor64 (%rsp)", "fp"),
    ("aarch64", "stp q0,q1,[sp,#288]", "fp"), ("aarch64", "fmov d0,x1", "fp"),
    ("aarch64", "ldr q0,[x1]", "fp"), ("aarch64", "movi v0.2d,#0", "fp"),
    ("aarch64", "mrs x9,fpcr", "fp"), ("aarch64", "msr fpsr,xzr", "fp"),
    ("aarch64", "msr cpacr_el1,x1", "enable"), ("aarch64", "mrs x8,cpacr_el1", None),
    ("aarch64", "ldp x0,x1,[sp]", None), ("aarch64", "b 0x40", None), ("aarch64", "add x0,x1,#8", None),
    ("aarch64", "str s0,[x0]", "fp"), ("aarch64", "msr tpidr_el0,x9", None),
    ("aarch64", "cnt v0.8b,v0.8b", "fp"), ("aarch64", "ucvtf d0,x0", "fp"),
    ("riscv64", "fld ft0,0(a0)", "fp"), ("riscv64", "fmv.x.d a0,fa0", "fp"),
    ("riscv64", "csrr a0,fcsr", "fp"), ("riscv64", "frcsr a0", "fp"), ("riscv64", "fence rw,rw", None),
    ("riscv64", "fence.i", None), ("riscv64", "vsetvli t0,a0,e8,m1,ta,ma", "fp"),
    ("riscv64", "csrrc a0,sstatus,a1", None), ("riscv64", "ld s0,0(sp)", None),
    ("riscv64", "add a0,a1,a2", None), ("riscv64", "csrw vstart,zero", "fp"),
]


def self_test():
    bad = 0
    for arch, text, want in SELF_TEST:
        got = classify(arch, normalise(arch, text))
        if got != want:
            bad += 1
            print(f"MISMATCH {arch} `{text}`: got {got}, want {want}")
    # every policy pattern must accept the instruction it was written for
    samples = {"X86_TRAP_ENTRY_CAPTURE": ["fxsave64 (%rsp)", "fninit",
                                          "ldmxcsr yarm_x86_kernel_mxcsr(%rip)"]}
    for kind, insns in samples.items():
        for pat, t in zip(POLICY["x86_64"][kind][1], insns):
            if not re.fullmatch(pat, t):
                bad += 1
                print(f"MISMATCH policy {kind}: `{t}` vs `{pat}`")
    print(f"NOFP_SELF_TEST cases={len(SELF_TEST)} mismatches={bad} result={'ok' if bad == 0 else 'fail'}")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", choices=["x86_64", "aarch64", "riscv64"])
    ap.add_argument("--elf")
    ap.add_argument("--features", default="")
    ap.add_argument("--toolchain", default="unrecorded")
    ap.add_argument("--boot-image")
    ap.add_argument("--census", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--max-lines", type=int, default=40)
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.arch or not a.elf:
        ap.error("--arch and --elf are required")
    if not os.path.isfile(a.elf):
        print(f"NOFP_CHECK arch={a.arch} image={a.elf} result=fail reason=inspection_incomplete:image_missing")
        return 1
    with open(a.elf, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    res = check(a.arch, a.elf, a.features, a.census, a.boot_image)
    dis = tool_version("objdump" if a.arch == "x86_64" else "llvm-objdump")
    sym = res["sym"]
    for e in res["exceptions"]:
        kind = e["name"].split(" ", 1)[0]
        print(f"  exception {kind:<24} {e['start']:#x}..{e['end']:#x} ({e['end'] - e['start']} bytes) "
              f"in {sym(e['start'])}")
    for r in res["regions"]:
        print(f"  region {r['name']:<28} {r['start']:#x}..{r['end']:#x} ({r['end'] - r['start']} bytes)")
    for r in res["eliminated"]:
        print(f"  eliminated-by-linker map record: {r['name']}")
    for line in res["report"]:
        print(f"  {line}")
    acct = res["acct"]
    print(f"  accounted: executable={acct['sections']} code={acct['code']} data={acct['data']} "
          f"user_program={acct['user']} unaccounted={acct['sections'] - acct['code'] - acct['data'] - acct['user']}")
    if acct["sections"] != acct["code"] + acct["data"] + acct["user"]:
        res["fails"].append("inspection incomplete: executable bytes not fully accounted")
    pops = res["pops"]
    total = sum(pops.values())
    ordinary = len(res["ordinary"])
    print(f"  population: classified={total} in_exceptions={total - ordinary} ordinary={ordinary}")
    for k in sorted(pops, key=lambda k: -pops[k])[:12]:
        print(f"    {pops[k]:>7} {k}")
    shown = 0
    for f in res["fails"]:
        if shown < a.max_lines:
            print(f"  FAIL {f}")
        shown += 1
    if shown > a.max_lines:
        print(f"  ... {shown - a.max_lines} more failure line(s)")
    verdict = "census" if a.census else ("pass" if not res["fails"] else "fail")
    print(f"NOFP_CHECK arch={a.arch} image_sha256={digest} features={','.join(sorted(res['feats'])) or '-'} "
          f"toolchain=\"{a.toolchain}\" disassembler=\"{dis}\" failures={len(res['fails'])} "
          f"ordinary_fp={ordinary} result={verdict}")
    return 0 if verdict == "pass" else (0 if a.census else 1)


if __name__ == "__main__":
    sys.exit(main())
