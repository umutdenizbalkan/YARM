#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KERNEL-NOFP1 — final-image check: ordinary kernel code uses no FP/SIMD.

    check-kernel-nofp.py --arch x86_64|aarch64|riscv64 --elf KERNEL_ELF
                         [--features LIST] [--toolchain TEXT] [--boot-image BIN]
    check-kernel-nofp.py --census ...      report populations only; never passes
    check-kernel-nofp.py --self-test       classifier, parser and policy unit checks
    check-kernel-nofp.py --list-inventory  print the reviewed exclusion inventory

What it inspects is the final linked ELF that boots (for AArch64, `--boot-image` additionally
proves the raw image QEMU loads is exactly this ELF's loadable bytes).

1. Every byte of every executable section is either DECODED or EXCLUDED from decoding.
   Excluded bytes are exactly (a) the x86 boot-mode DATA ranges, (b) embedded USER-MODE programs
   the kernel copies out and never executes, and (c) AArch64 literal-pool words. The x86 16-bit
   and 32-bit boot code is not excluded: it is decoded in its own mode.
2. An exclusion is never taken from the image's annotation alone. The image's `.yarm_nofp_map`
   table (a non-loaded section the sources emit) only NAMES a candidate range. It is accepted
   only if its name is in the closed, reviewed inventory below (EXCLUSIONS), and its bounds are
   exactly that entry's anchors — the ELF symbols and sections the owning source defines (the
   symbols the kernel itself uses to copy a program out, the trampoline's own labels, the boot
   section's bounds). The entry also fixes its decoding mode, its section, the cargo feature
   that compiles it (read from Cargo.toml's feature graph), whether the linker may eliminate it,
   and boot-mode constraints. No sized function or object symbol may intersect it. A record
   that fails any of this excludes nothing: its bytes are decoded as ordinary code.
3. Every decoded range is disassembled IN FULL, in its mode, zero blocks included. The decoded
   instructions must tile the range exactly. Every output line must be one the checker can
   interpret: an undecodable byte, a gap, a missing disassembler, an unrecognised output line,
   an unknown instruction prefix or a malformed mnemonic is a failure ("inspection incomplete"),
   never a pass. x86 prefixes are parsed with the decoder's own closed prefix grammar (GNU
   objdump 2.42 / libopcodes: every REX form, REX2, operand/address size, segment, LOCK/REP
   family, XACQUIRE/XRELEASE, BND, NOTRACK, {vex}/{evex}), so classification reaches the
   instruction after any legitimate prefix combination. A data directive the disassembler emits
   inside decoded code is accepted only where it is justified as data: on AArch64, inside an
   assembler `$d` mapping span AND covered by the target of a PC-relative literal load in
   decoded code; on x86 and RISC-V never.
4. Each instruction is classified: FP/SIMD (an FP/SIMD/vector register operand, an x87/FP/vector
   mnemonic, or an implicit-state instruction such as FXSAVE/LDMXCSR/XSAVE/EMMS), FP control
   state (MXCSR, FCW, FPCR/FPSR, fcsr/frm/fflags, vector CSRs), or FP-enable control
   (CR0/CR4/XCR0 writes, CLTS, LMSW; CPACR_EL1/CPTR_ELx writes).
5. Every classified instruction must lie inside an audited EXCEPTION record of the map. Each
   exception kind names its purpose; the checker holds, per kind, the exact ordered list of
   FP/control instructions it may contain, a bound on its total size, and how many instances the
   build must carry. Anything else inside an exception range (an unrelated FP operation, a missing
   save, a reordered restore) fails it.

What this establishes, and what it does not: for the decoded bytes, the disassembler's linear
decoding contains no FP/SIMD instruction outside the audited exceptions; the excluded bytes are
exactly the reviewed inventory's anchored ranges. It does NOT prove arbitrary control-flow safety
(that execution never enters an excluded range or the middle of a decoded instruction), and it
does not prove the disassembler correct: it trusts GNU objdump / LLVM objdump to decode what the
CPU executes, and records which version it trusted.

Every failure carries an obligation code (`[NOFP-...]`); the result line lists the codes and binds
the verdict to the image hash, the features, the toolchain and the disassembler.
"""

import argparse
import bisect
import hashlib
import os
import re
import shutil
import struct
import subprocess
import sys

MAGIC = 0x3150464F4E4D5259  # b"YRMNOFP1", little-endian
MAP_SECTION = ".yarm_nofp_map"
REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

# Obligation codes: every failure names exactly one.
OBLIGATIONS = {
    "NOFP-ORDINARY": "an FP/SIMD or FP-enable instruction lies outside every audited exception",
    "NOFP-EXCEPTION": "an exception range holds other than its kind's exact instruction list",
    "NOFP-INVENTORY": "an exception kind has the wrong number of instances for this build",
    "NOFP-DECODE": "inspection incomplete: a byte was not decoded, or decoder output not understood",
    "NOFP-DATA": "the disassembler emitted data inside decoded code that is not justified as data",
    "NOFP-EXCL-UNKNOWN": "a map record names no reviewed exclusion or exception kind",
    "NOFP-EXCL-ANCHOR": "an exclusion's bounds are not exactly its reviewed anchors",
    "NOFP-EXCL-PRESENCE": "an exclusion is missing, duplicated, or present against its feature",
    "NOFP-EXCL-OWNERSHIP": "an exclusion covers bytes owned by kernel functions or objects",
    "NOFP-EXCL-OVERLAP": "map records overlap or classify the same bytes in conflicting ways",
    "NOFP-EXCL-MODE": "an exclusion is inconsistent with its architecture or boot mode",
    "NOFP-METADATA": "the image, its map table or the feature list is malformed",
    "NOFP-BOOT-IMAGE": "the raw boot image is not exactly the ELF's loadable bytes",
    "NOFP-TOOLS": "the required disassembler is missing, unselectable, unsupported or mis-decodes",
}

# ── ELF ─────────────────────────────────────────────────────────────────────────────────────────

SHF_ALLOC = 0x2
SHF_EXECINSTR = 0x4
SHT_NOBITS = 8
SHT_SYMTAB = 2
PT_LOAD = 1
PF_X = 1
STT_OBJECT, STT_FUNC = 1, 2
E_MACHINE = {"x86_64": 62, "aarch64": 183, "riscv64": 243}


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
                                         size=size, type=info & 0xF, bind=info >> 4, shndx=shndx))

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


RECORD_NAME = re.compile(r"[A-Z][A-Z0-9_]*(?: [a-z][a-z0-9_]*)?")


def parse_map(elf):
    """The image's own exception/region table. Returns (records, problems)."""
    s = elf.section(MAP_SECTION)
    if s is None:
        return [], []
    if s["flags"] & SHF_ALLOC:
        return [], [f"map: {MAP_SECTION} is a loaded section; it must be annotation only"]
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
        name = raw[i + 24:j].decode("latin-1")
        if not RECORD_NAME.fullmatch(name):
            problems.append(f"map: malformed record name {name!r} at offset {i:#x}")
        recs.append(dict(start=start, end=end, name=name))
        i = (j + 1 + 7) & ~7
    if i < len(raw) and not problems:
        problems.append(f"map: {len(raw) - i} trailing byte(s) at offset {i:#x}")
    return recs, problems


# ── disassembly ─────────────────────────────────────────────────────────────────────────────────

RV_MATTR = "+m,+a,+f,+d,+c,+v,+zicsr,+zifencei,+zihintpause,+zfh,+zfhmin,+zba,+zbb,+zbs"
# `addr: <raw> <text>`; raw is byte pairs (GNU x86, LLVM RISC-V) or whole 16/32-bit words (LLVM
# AArch64, and RISC-V `.short`/`.word` data).
LINE = re.compile(r"^\s*([0-9a-f]+):\s+((?:(?:[0-9a-f]{2}|[0-9a-f]{4}|[0-9a-f]{8})\s)+)\s*(.*)$")
# The only other lines either disassembler prints for `-d -z` over one section.
SKIP_LINE = re.compile(r"^(?:\s*|.*:\s+file format \S+|Disassembly of section \S+:|"
                       r"[0-9a-f]+ <[^<>]*>:)$")
DIRECTIVE = re.compile(r"^\.(byte|short|hword|2byte|word|long|4byte|quad|8byte|inst)\b")


class Uninterpretable(Exception):
    pass


# ── the disassemblers: one resolution, one support gate, used by every caller ───────────────────
#
# BUILD-GATES1: the CLI fixtures, the artifact builds and CI all resolve the disassemblers HERE.
#
#   * Explicit selection wins: `YARM_OBJDUMP` (GNU objdump, x86) and `YARM_LLVM_OBJDUMP` (LLVM
#     objdump, AArch64 and RISC-V) name the binaries; CI sets both. Otherwise `objdump` and
#     `llvm-objdump` are taken from PATH. Nothing else is guessed (no versioned-name search).
#   * Only the versions this checker's grammar was derived from are supported: GNU objdump 2.42
#     (its x86 prefix table is the grammar `x86_is_prefix` enforces) and LLVM 18 objdump.
#   * Before an image is inspected, the selected binary must decode a built-in probe image for
#     the arch exactly as expected (x86-64/i386/i8086 for x86, the FP/SIMD forms the policy names
#     for AArch64, F/D/V and FP-CSR forms for RISC-V).
#
# A missing, unselectable, unsupported or mis-decoding disassembler is a NOFP-TOOLS failure with
# the reason and the remedy; it is never skipped and never replaced by another binary.

TOOL_ENV = {"objdump": "YARM_OBJDUMP", "llvm-objdump": "YARM_LLVM_OBJDUMP"}
TOOL_FOR_ARCH = {"x86_64": "objdump", "aarch64": "llvm-objdump", "riscv64": "llvm-objdump"}
TOOL_SUPPORTED = {
    "objdump": (re.compile(r"^GNU objdump .*\b2\.42(?:\.\d+)?$"), "GNU objdump 2.42 (binutils 2.42)"),
    "llvm-objdump": (re.compile(r"\bLLVM version 18\.\d+\.\d+\b"), "LLVM 18 llvm-objdump"),
}
TOOL_REMEDY = ("install binutils 2.42 and LLVM 18 (Ubuntu 24.04: `apt-get install binutils "
               "llvm-18`), then put `objdump`/`llvm-objdump` on PATH or select them explicitly "
               "with YARM_OBJDUMP=/usr/bin/objdump YARM_LLVM_OBJDUMP=/usr/lib/llvm-18/bin/"
               "llvm-objdump")


class ToolError(Exception):
    pass


_TOOLS = {}


def tool(name):
    """The selected binary for `name` (path only; `require_tool` also gates support)."""
    if name in _TOOLS:
        return _TOOLS[name]["path"]
    env = TOOL_ENV[name]
    sel = os.environ.get(env)
    if sel:
        p = sel if os.path.sep in sel else shutil.which(sel)
        if not p or not (os.path.isfile(p) and os.access(p, os.X_OK)):
            raise ToolError(f"{env}={sel} does not name an executable disassembler; {TOOL_REMEDY}")
        how = env
    else:
        p = shutil.which(name)
        if not p:
            raise ToolError(f"required disassembler `{name}` is not on PATH and {env} is not set; "
                            f"{TOOL_REMEDY}")
        how = "PATH"
    return os.path.abspath(p) if os.path.sep in p else p


def tool_version_of(path):
    r = subprocess.run([path, "--version"], capture_output=True, text=True)
    lines = [l.strip() for l in r.stdout.splitlines() if l.strip()]
    if r.returncode != 0 or not lines:
        raise ToolError(f"`{path} --version` failed (exit {r.returncode}); {TOOL_REMEDY}")
    rx, _ = TOOL_SUPPORTED["llvm-objdump" if "LLVM" in r.stdout else "objdump"]
    for l in lines:  # LLVM prints its version on the second line
        if rx.search(l):
            return l
    return lines[0]


def _probe_elf(machine, eflags, code):
    """A minimal ELF64 with one executable `.text` at 0x1000 holding `code`."""
    shstr = b"\0.text\0.shstrtab\0"
    text_off, shstr_off = 0x100, 0x100 + len(code)
    shoff = (shstr_off + len(shstr) + 7) & ~7
    hdr = (b"\x7fELF\x02\x01\x01\0" + b"\0" * 8 +
           struct.pack("<HHIQQQIHHHHHH", 2, machine, 1, 0x1000, 0, shoff, eflags, 64, 56, 0, 64,
                       3, 2))
    out = bytearray(shoff + 3 * 64)
    out[:64] = hdr
    out[text_off:text_off + len(code)] = code
    out[shstr_off:shstr_off + len(shstr)] = shstr
    out[shoff + 64:shoff + 128] = struct.pack("<IIQQQQIIQQ", 1, 1, 6, 0x1000, text_off, len(code),
                                              0, 0, 4, 0)
    out[shoff + 128:shoff + 192] = struct.pack("<IIQQQQIIQQ", 7, 3, 0, 0, shstr_off, len(shstr),
                                               0, 0, 1, 0)
    return bytes(out)


# arch -> [(decoding mode, code bytes, expected normalised instruction texts)]
TOOL_PROBES = {
    "x86_64": [("code64", bytes.fromhex("49dbe3d50090480fae10"),
                ["rex.wb fninit", "{rex2 0x0} nop", "rex.w ldmxcsr (%rax)"]),
               ("code32", bytes.fromhex("0f22c0dbe3"), ["mov %eax,%cr0", "fninit"]),
               ("code16", bytes.fromhex("66b8100000000f22c0"), ["mov $0x10,%eax", "mov %eax,%cr0"])],
    "aarch64": [("native", struct.pack("<III", 0xAD0907E0, 0xD5181041, 0x9E670020),
                 ["stp q0,q1,[sp,#288]", "msr cpacr_el1,x1", "fmov d0,x1"])],
    "riscv64": [("native", struct.pack("<III", 0x00053007, 0x0C0572D7, 0x00302573),
                 ["fld ft0,0(a0)", "vsetvli t0,a0,e8,m1,ta,ma", "frcsr a0"])],
}
PROBE_MACHINE = {"x86_64": (62, 0), "aarch64": (183, 0), "riscv64": (243, 0x5)}


def require_tool(arch):
    """Resolve, version-gate and probe the disassembler `arch` needs. Returns its record."""
    name = TOOL_FOR_ARCH[arch]
    rec = _TOOLS.get(name)
    if rec is None:
        path = tool(name)
        version = tool_version_of(path)
        rx, supported = TOOL_SUPPORTED[name]
        if not rx.search(version):
            raise ToolError(f"{path} reports `{version}`; the supported disassembler is "
                            f"{supported}; {TOOL_REMEDY}")
        rec = dict(name=name, path=path, version=version,
                   selected_by=TOOL_ENV[name] if os.environ.get(TOOL_ENV[name]) else "PATH",
                   probed=set())
        _TOOLS[name] = rec
    if arch not in rec["probed"]:
        import tempfile
        machine, eflags = PROBE_MACHINE[arch]
        for mode, code, want in TOOL_PROBES[arch]:
            fd, p = tempfile.mkstemp(prefix="nofp-probe-", suffix=".elf")
            try:
                with os.fdopen(fd, "wb") as f:
                    f.write(_probe_elf(machine, eflags, code))
                try:
                    got = [normalise(arch, t) for _, _, t, _ in
                           disassemble(arch, p, ".text", 0x1000, 0x1000 + len(code), mode)]
                except SystemExit as e:
                    raise ToolError(f"{rec['path']} cannot decode the {arch} {mode} probe: {e}; "
                                    f"{TOOL_REMEDY}")
            finally:
                os.unlink(p)
            if got != want:
                raise ToolError(f"{rec['path']} decodes the {arch} {mode} probe as {got}, expected "
                                f"{want}; this checker's grammar does not fit it; {TOOL_REMEDY}")
        rec["probed"].add(arch)
    return rec


def tool_version(name):
    return _TOOLS[name]["version"] if name in _TOOLS else tool_version_of(tool(name))


def disassemble(arch, elf_path, section, lo, hi, mode):
    """Instructions of [lo, hi) as (addr, length, text, directive-or-None). Raises SystemExit
    (inspection incomplete) on any output line it cannot interpret."""
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
            if SKIP_LINE.match(line) or line.strip() == elf_path + ":":
                continue
            raise SystemExit(f"[nofp] inspection incomplete: unrecognised disassembler output "
                             f"line {line[:120]!r}")
        addr = int(m.group(1), 16)
        nbytes = sum(len(tok) // 2 for tok in m.group(2).split())
        text = m.group(3).strip()
        if arch == "x86_64" and not text and out:
            # GNU objdump continues a long instruction's bytes on the next line.
            a, n, t, dd = out[-1]
            if a + n != addr:
                raise SystemExit(f"[nofp] inspection incomplete: continuation line at {addr:#x} "
                                 f"does not follow {a:#x}")
            out[-1] = (a, n + nbytes, t, dd)
            continue
        d = DIRECTIVE.match(text)
        out.append((addr, nbytes, text, d.group(1) if d else None))
    return out


# ── instruction text ────────────────────────────────────────────────────────────────────────────

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


# The x86 decoder's own prefix names (GNU objdump 2.42, libopcodes i386-dis.c `prefix_name` and
# its pseudo-prefixes), per decoding mode. Anything else in prefix position is not interpreted.
X86_PREFIX_ANY_MODE = {"lock", "rep", "repz", "repnz", "xacquire", "xrelease", "bnd", "notrack",
                       "cs", "ds", "ss", "es", "fs", "gs", "{vex}", "{vex3}", "{evex}"}
X86_PREFIX_BY_MODE = {"code64": {"data16", "addr32"}, "code32": {"data16", "addr16"},
                      "code16": {"data32", "addr32"}}
X86_REX = re.compile(r"rex(?:\.(?=[wrxb])w?r?x?b?)?")          # rex, rex.b ... rex.wrxb (16 forms)
X86_REX2 = re.compile(r"\{rex2 0x[0-9a-f]{1,2}\}")
X86_TOKEN = re.compile(r"\{[^{}]*\}(?=\s)|\S+")
X86_MNEMONIC = re.compile(r"[a-z][a-z0-9]*(?:,p[nt])?")          # `je,pt`: branch-hint suffix
# An AT&T operand list starts with a register, immediate, indirection, memory reference, an
# address (decimal, 0x, or a full-width hex address) or `SYM(%rip)` (from `normalise`) — never
# with a bare word, which would be an uninterpreted mnemonic after an unknown prefix.
X86_OPERANDS = re.compile(r"(?:[%$*(\-{]|[0-9]|[0-9a-f]{8,}(?![0-9a-z_])|[a-z_.][\w.@$]*\(%rip\))\S*")
RISC_MNEMONIC = re.compile(r"[a-z][a-z0-9]*(?:\.[a-z0-9]+)*")


def x86_is_prefix(tok, mode):
    if tok in X86_PREFIX_ANY_MODE or tok in X86_PREFIX_BY_MODE[mode]:
        return True
    return mode == "code64" and bool(X86_REX.fullmatch(tok) or X86_REX2.fullmatch(tok))


def parse(arch, norm, mode="native"):
    """(prefixes, mnemonic, operands). The mnemonic is None for a prefix-only line (a prefix the
    decoder printed alone because the instruction after it ignores it). Raises Uninterpretable
    for any form the checker does not understand."""
    if arch == "x86_64":
        mode = "code64" if mode == "native" else mode
        toks = X86_TOKEN.findall(norm)
        prefixes = []
        while toks and x86_is_prefix(toks[0], mode):
            prefixes.append(toks.pop(0))
        if not toks:
            if not prefixes:
                raise Uninterpretable("empty instruction text")
            return prefixes, None, ""
        mnem = toks.pop(0)
        if not X86_MNEMONIC.fullmatch(mnem):
            raise Uninterpretable(f"`{mnem}` is neither a known prefix nor a mnemonic")
        if len(toks) > 1:
            raise Uninterpretable(f"unexpected tokens after `{mnem}`: {' '.join(toks)}")
        if toks and not X86_OPERANDS.fullmatch(toks[0]):
            raise Uninterpretable(f"`{toks[0]}` after `{mnem}` is not an operand list")
        return prefixes, mnem.split(",")[0], toks[0] if toks else ""
    p = norm.split(" ", 1)
    if not RISC_MNEMONIC.fullmatch(p[0]):
        raise Uninterpretable(f"`{p[0]}` is not a mnemonic")
    return [], p[0], p[1] if len(p) > 1 else ""


X86_FP_REG = re.compile(r"%(?:[xyz]mm\d+|mm[0-7]|st(?:\(\d\))?|k[0-7]|tmm[0-7])\b")
X86_FP_IMPLICIT = {"emms", "femms", "vzeroupper", "vzeroall", "ldmxcsr", "stmxcsr", "vldmxcsr",
                   "vstmxcsr", "wait", "xsave", "xsave64", "xsaveopt", "xsaveopt64", "xsavec",
                   "xsavec64", "xsaves", "xsaves64", "xrstor", "xrstor64", "xrstors", "xrstors64",
                   "ldtilecfg", "sttilecfg", "tilerelease", "tilezero"}
X86_CTRL = re.compile(r"^mov[lq]?$")

A64_FP_REG = re.compile(r"(?<![\w.#])(?:[qdshb](?:[12]?\d|3[01])|v(?:[12]?\d|3[01])(?:\.\d*[bhsdq])?|"
                        r"z(?:[12]?\d|3[01])|p(?:1[0-5]|\d)|za)(?![\w])")
A64_FP_SYSREG = re.compile(r"\b(fpcr|fpsr|fpmr|zcr_el[123]|smcr_el[123]|svcr|tpidr2_el0)\b")
A64_FP_ENABLE = re.compile(r"\b(cpacr_el1|cpacr_el12|cptr_el2|cptr_el3)\b")

RV_FP_REG = re.compile(r"(?<![\w.])(?:f(?:[12]?\d|3[01])|ft(?:1[01]|\d)|fs(?:1[01]|\d)|fa[0-7]|"
                       r"v(?:[12]?\d|3[01]))(?![\w])")
RV_FP_CSR = re.compile(r"\b(fflags|frm|fcsr|vstart|vxsat|vxrm|vcsr|vl|vtype|vlenb)\b")


def classify(arch, norm, mode="native"):
    """'fp' (FP/SIMD/vector use or FP control state), 'enable' (FP-enable control write), or None.
    Raises Uninterpretable when the text is not a form the checker understands."""
    _, mnem, ops = parse(arch, norm, mode)
    if mnem is None:
        return None
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


def split(t, arch="x86_64", mode="native"):
    """(mnemonic, operands) after prefixes; for reports."""
    try:
        _, mnem, ops = parse(arch, t, mode)
    except Uninterpretable:
        return t.split(" ", 1)[0], ""
    return mnem or "<prefix>", ops


# AArch64 PC-relative literal loads: the only thing that justifies data inside decoded code.
A64_LITERAL_LOAD = re.compile(r"(x|w|s|d|q)\d+,(0x[0-9a-f]+|\d+)")
A64_LITERAL_SIZE = {"x": 8, "w": 4, "s": 4, "d": 8, "q": 16}


def a64_literal_target(norm):
    try:
        _, mnem, ops = parse("aarch64", norm)
    except Uninterpretable:
        return None
    if mnem not in ("ldr", "ldrsw"):
        return None
    m = A64_LITERAL_LOAD.fullmatch(ops)
    if not m:
        return None
    size = 4 if mnem == "ldrsw" else A64_LITERAL_SIZE[m.group(1)]
    return int(m.group(2), 0), size


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


# ── the reviewed exclusion inventory ────────────────────────────────────────────────────────────
#
# The ONLY ranges the check may leave undecoded (DATA, USER_PROGRAM) or decode in another mode
# (CODE16, CODE32). Keyed by the exact map record name. Each entry fixes:
#   mode       how the bytes are treated: code16 / code32 (decoded in that mode), data / user
#              (not decoded);
#   purpose    why the bytes are what the mode says;
#   source     the source file that owns the bytes and emits the record;
#   section    the output section the bytes must lie in;
#   segments   the anchors, as (start, end) pairs that must tile the range in order: a symbol name,
#              or ("section-start"|"section-end", NAME). These are the symbols the owning source
#              defines and the kernel uses (the copy-out bounds of a user program, the
#              trampoline's own labels), never the map record's own labels;
#   feature    the cargo feature that compiles the source (None: every build), resolved through
#              Cargo.toml's feature graph;
#   eliminable whether the linker may garbage-collect the bytes (record then reads 0..0 and no
#              anchor symbol may exist);
#   functions  zero-size STT_FUNC symbols permitted inside (asm entry points of the range itself);
#   boot       boot-mode constraints: "identity-below-4g" (32-bit code runs at its link address
#              before paging), ("blob", START, END, MAX) (the range is part of a blob copied to one
#              low page and entered in real mode).

TRAMPOLINE_BLOB = ("blob", "yarm_ap_trampoline_start", "yarm_ap_trampoline_end", 4096)


def user(source, segs, feature):
    return dict(mode="user", purpose="embedded user-mode witness program: copied into a task's "
                "address space, executed only at EL0/ring 3/U-mode, never by the kernel",
                source=source, section=".text", segments=segs, feature=feature, eliminable=True,
                functions=set(), boot=None)


EXCLUSIONS = {
    "x86_64": {
        "CODE32 boot32": dict(
            mode="code32", purpose="PVH entry: 32-bit protected-mode boot code before long mode",
            source="src/arch/x86_64/boot.rs", section=".text.boot32",
            segments=[(("section-start", ".text.boot32"), ("section-end", ".text.boot32"))],
            feature=None, eliminable=False, functions={"pvh_start32"}, boot="identity-below-4g"),
        "CODE16 ap_trampoline": dict(
            mode="code16", purpose="AP startup trampoline: real-mode code entered by SIPI",
            source="src/arch/x86_64/smp_trampoline.rs", section=".text",
            segments=[("yarm_ap_trampoline_start", "ap_gdtr")], feature=None, eliminable=False,
            functions=set(), boot=TRAMPOLINE_BLOB),
        "DATA ap_trampoline_gdt": dict(
            mode="data", purpose="AP startup trampoline: the GDTR image and the GDT it loads",
            source="src/arch/x86_64/smp_trampoline.rs", section=".text",
            segments=[("ap_gdtr", "ap_protected_entry")], feature=None, eliminable=False,
            functions=set(), boot=TRAMPOLINE_BLOB),
        "CODE32 ap_trampoline": dict(
            mode="code32", purpose="AP startup trampoline: 32-bit protected-mode code to long mode",
            source="src/arch/x86_64/smp_trampoline.rs", section=".text",
            segments=[("ap_protected_entry", "ap_long_entry")], feature=None, eliminable=False,
            functions=set(), boot=TRAMPOLINE_BLOB),
        "DATA ap_trampoline_handoff": dict(
            mode="data", purpose="AP startup trampoline: the handoff block the BSP fills",
            source="src/arch/x86_64/smp_trampoline.rs", section=".text",
            segments=[("yarm_ap_trampoline_handoff", "yarm_ap_trampoline_end")], feature=None,
            eliminable=False, functions=set(), boot=TRAMPOLINE_BLOB),
        "USER_PROGRAM smp1_witness": user(
            "src/arch/x86_64/smp1_witness.S",
            [("yarm_smp1_server_start", "yarm_smp1_server_end"),
             ("yarm_smp1_client_start", "yarm_smp1_client_end")], "x86-smp1-witness"),
        "USER_PROGRAM apsched_witness": user(
            "src/arch/x86_64/apsched_witness.S", [("yarm_apsw_start", "yarm_apsw_end")],
            "apsched-witness"),
        "USER_PROGRAM overtaken_witness": user(
            "src/arch/x86_64/overtaken_witness.S",
            [("yarm_ovt_x86_w_start", "yarm_ovt_x86_w_end"),
             ("yarm_ovt_x86_k_start", "yarm_ovt_x86_k_end")], "x86-overtaken-witness"),
        "USER_PROGRAM vmi2_witness": user(
            "src/arch/x86_64/vmi2_witness.S", [("yarm_vmi2_start", "yarm_vmi2_end")],
            "x86-vmi2-witness"),
    },
    "aarch64": {
        "USER_PROGRAM smp2_witness": user(
            "src/arch/aarch64/smp2_witness.S",
            [("yarm_smp2_s_start", "yarm_smp2_s_end"), ("yarm_smp2_c_start", "yarm_smp2_c_end"),
             ("yarm_smp2_h_start", "yarm_smp2_h_end")], "aarch64-smp2-witness"),
        "USER_PROGRAM apsched_witness": user(
            "src/arch/aarch64/apsched_witness.S", [("yarm_apsw_start", "yarm_apsw_end")],
            "apsched-witness"),
        "USER_PROGRAM overtaken_witness": user(
            "src/arch/aarch64/overtaken_witness.S",
            [("yarm_ovt_w_start", "yarm_ovt_w_end"), ("yarm_ovt_k_start", "yarm_ovt_k_end")],
            "aarch64-overtaken-witness"),
    },
    "riscv64": {
        "USER_PROGRAM smp3_witness": user(
            "src/arch/riscv64/smp3_witness.S",
            [("yarm_smp3_s_start", "yarm_smp3_s_end"), ("yarm_smp3_c_start", "yarm_smp3_c_end"),
             ("yarm_smp3_h_start", "yarm_smp3_h_end")], "riscv64-smp3-witness"),
        "USER_PROGRAM apsched_witness": user(
            "src/arch/riscv64/apsched_witness.S", [("yarm_apsw_start", "yarm_apsw_end")],
            "apsched-witness"),
        "USER_PROGRAM sr_replace_witness": user(
            "src/arch/riscv64/sr_replace_witness.S",
            [("yarm_srr_r_start", "yarm_srr_r_end"), ("yarm_srr_o_start", "yarm_srr_o_end")],
            "riscv64-sr-replace-witness"),
        "USER_PROGRAM vmi_witness": user(
            "src/arch/riscv64/vmi_witness.S",
            [("yarm_vmiw_r_start", "yarm_vmiw_r_end"), ("yarm_vmiw_o_start", "yarm_vmiw_o_end")],
            "riscv64-vmi-witness"),
    },
}
REGION_HEADS = {"CODE16", "CODE32", "DATA", "USER_PROGRAM"}


# ── features ────────────────────────────────────────────────────────────────────────────────────

def cargo_features(path):
    import tomllib
    with open(path, "rb") as f:
        return tomllib.load(f).get("features", {})


def expand(features, graph):
    """The requested features closed under Cargo.toml's implications. Unknown names fail."""
    feats = {f.strip() for f in features.replace(" ", ",").split(",") if f.strip()}
    unknown = sorted(f for f in feats if f not in graph)
    todo = list(feats)
    while todo:
        f = todo.pop()
        for g in graph.get(f, ()):
            if g.startswith("dep:") or "/" in g or g in feats:
                continue
            feats.add(g)
            todo.append(g)
    return feats, unknown


# ── the check ───────────────────────────────────────────────────────────────────────────────────

def symbolizer(elf):
    funcs = sorted((s["value"], s["name"]) for s in elf.symbols
                   if s["name"] and not s["name"].startswith("$") and s["type"] in (0, 1, 2))
    addrs = [a for a, _ in funcs]

    def at(addr):
        i = bisect.bisect_right(addrs, addr) - 1
        if i < 0:
            return f"{addr:#x}"
        return f"{funcs[i][1]}+{addr - funcs[i][0]:#x}"
    return at


def mapping_spans(elf):
    """Per section index: sorted (addr, 'x'|'d') from the assembler's mapping symbols."""
    out = {}
    for s in elf.symbols:
        m = re.fullmatch(r"\$([xd])(?:\..*)?", s["name"])
        if m and s["shndx"] not in (0, 0xFFF1):
            out.setdefault(s["shndx"], []).append((s["value"], m.group(1)))
    for v in out.values():
        v.sort()
    return out


def in_data_span(spans, shndx, addr, n):
    v = spans.get(shndx, [])
    i = bisect.bisect_right(v, (addr, "z")) - 1
    if i < 0 or v[i][1] != "d":
        return False
    return not any(addr < a < addr + n and k != "d" for a, k in v)


class Failures(list):
    def add(self, code, text):
        assert code in OBLIGATIONS, code
        self.append((code, text))


def authenticate(arch, elf, records, feats, fails):
    """Accept the map's region records that match the reviewed inventory; everything else
    excludes nothing. Returns the accepted regions (each with `mode`)."""
    inv = EXCLUSIONS[arch]
    syms = {}
    for s in elf.symbols:
        if s["name"] and not s["name"].startswith("$") and s["type"] in (0, 1, 2):
            syms.setdefault(s["name"], []).append(s)
    by_name = {}
    for r in records:
        if r["name"] not in inv:
            fails.add("NOFP-EXCL-UNKNOWN", f"map record `{r['name']}` names no reviewed "
                      f"{arch} exclusion; it excludes nothing")
            continue
        by_name.setdefault(r["name"], []).append(r)

    def resolve(anchor, ent):
        if isinstance(anchor, tuple):
            sec = elf.section(anchor[1])
            if sec is None:
                return None, f"section {anchor[1]} missing"
            return (sec["addr"] if anchor[0] == "section-start" else sec["addr"] + sec["size"]), None
        found = [s for s in syms.get(anchor, []) if s["shndx"] not in (0, 0xFFF1)]
        if len(found) != 1:
            return None, f"anchor symbol `{anchor}` defined {len(found)} times"
        sec = elf.sections[found[0]["shndx"]]
        if sec["name"] != ent["section"]:
            return None, f"anchor `{anchor}` lies in {sec['name']}, not {ent['section']}"
        return found[0]["value"], None

    def anchor_names(ent):
        return [a for seg in ent["segments"] for a in seg if not isinstance(a, tuple)]

    accepted = []
    for name, ent in inv.items():
        rs = by_name.get(name, [])
        live = [r for r in rs if not (r["start"] == 0 and r["end"] == 0)]
        gone = [r for r in rs if r["start"] == 0 and r["end"] == 0]
        compiled = ent["feature"] is None or ent["feature"] in feats
        present = [a for a in anchor_names(ent) if syms.get(a)]
        if not compiled:
            if rs:
                fails.add("NOFP-EXCL-PRESENCE", f"`{name}` is recorded but its feature "
                          f"`{ent['feature']}` is not in this build")
            if present:
                fails.add("NOFP-EXCL-PRESENCE", f"`{name}`: anchor symbol(s) {present} exist "
                          f"although feature `{ent['feature']}` is not in this build")
            continue
        if not rs:
            fails.add("NOFP-EXCL-PRESENCE", f"required exclusion record `{name}` is missing")
            continue
        if len(rs) > 1:
            fails.add("NOFP-EXCL-PRESENCE", f"exclusion record `{name}` appears {len(rs)} times")
            continue
        if gone:
            if not ent["eliminable"]:
                fails.add("NOFP-EXCL-PRESENCE", f"`{name}` reads as linker-eliminated, but it is "
                          f"always linked")
            elif present:
                fails.add("NOFP-EXCL-PRESENCE", f"`{name}` reads as linker-eliminated, but its "
                          f"anchor symbol(s) {present} are linked")
            continue
        r = live[0]
        bounds, problems = [], []
        for a, b in ent["segments"]:
            lo, e1 = resolve(a, ent)
            hi, e2 = resolve(b, ent)
            problems += [e for e in (e1, e2) if e]
            bounds.append((lo, hi))
        if not problems:
            for i, (lo, hi) in enumerate(bounds):
                if not lo < hi:
                    problems.append(f"segment {i} anchors are empty or inverted")
                if i and bounds[i - 1][1] != lo:
                    problems.append(f"segment {i} does not start where segment {i - 1} ends")
        if problems:
            fails.add("NOFP-EXCL-ANCHOR", f"`{name}`: {'; '.join(problems)}")
            continue
        lo, hi = bounds[0][0], bounds[-1][1]
        if (r["start"], r["end"]) != (lo, hi):
            fails.add("NOFP-EXCL-ANCHOR", f"`{name}` record {r['start']:#x}..{r['end']:#x} is not "
                      f"its anchored range {lo:#x}..{hi:#x}; it excludes nothing")
            continue
        sec = elf.section(ent["section"])
        if not (sec and sec["flags"] & SHF_EXECINSTR and sec["addr"] <= lo
                and hi <= sec["addr"] + sec["size"]):
            fails.add("NOFP-EXCL-ANCHOR", f"`{name}` {lo:#x}..{hi:#x} is not inside {ent['section']}")
            continue
        bad = False
        for s in elf.symbols:
            if s["shndx"] != sec["idx"] or s["name"].startswith("$") or s["type"] not in (STT_FUNC, STT_OBJECT):
                continue
            if s["size"] and s["value"] < hi and s["value"] + s["size"] > lo:
                fails.add("NOFP-EXCL-OWNERSHIP", f"`{name}` {lo:#x}..{hi:#x} covers kernel "
                          f"{'function' if s['type'] == STT_FUNC else 'object'} `{s['name']}` "
                          f"{s['value']:#x}..{s['value'] + s['size']:#x}")
                bad = True
            elif not s["size"] and lo <= s["value"] < hi and s["name"] not in ent["functions"]:
                fails.add("NOFP-EXCL-OWNERSHIP", f"`{name}` {lo:#x}..{hi:#x} covers kernel entry "
                          f"point `{s['name']}` at {s['value']:#x}")
                bad = True
        if ent["boot"] == "identity-below-4g":
            seg = [p for p in elf.segments if p["type"] == PT_LOAD and p["vaddr"] <= lo
                   and hi <= p["vaddr"] + p["memsz"]]
            if not seg or seg[0]["vaddr"] != seg[0]["paddr"] or hi > 1 << 32:
                fails.add("NOFP-EXCL-MODE", f"`{name}` is 32-bit boot code but {lo:#x}..{hi:#x} "
                          f"is not identity-loaded below 4 GiB")
                bad = True
        elif isinstance(ent["boot"], tuple):
            _, bs, be, mx = ent["boot"]
            b0, e1 = resolve(bs, ent)
            b1, e2 = resolve(be, ent)
            if e1 or e2 or not (b0 <= lo and hi <= b1) or b1 - b0 > mx:
                fails.add("NOFP-EXCL-MODE", f"`{name}` must lie inside the real-mode blob "
                          f"{bs}..{be} of at most {mx} bytes")
                bad = True
        if ent["mode"] in ("code16", "code32") and arch != "x86_64":
            fails.add("NOFP-EXCL-MODE", f"`{name}`: {ent['mode']} on {arch}")
            bad = True
        if bad:
            continue
        accepted.append(dict(r, mode=ent["mode"], section=sec["name"], entry=ent))
    return accepted


def check(arch, elf_path, features, census=False, boot_image=None, cargo_toml=None):
    require_tool(arch)
    elf = Elf(elf_path)
    fails = Failures()
    if elf.e_machine != E_MACHINE[arch]:
        fails.add("NOFP-METADATA", f"ELF e_machine {elf.e_machine} is not {arch} "
                  f"({E_MACHINE[arch]})")
    graph = cargo_features(cargo_toml or os.path.join(REPO, "Cargo.toml"))
    feats, unknown = expand(features, graph)
    for f in unknown:
        fails.add("NOFP-METADATA", f"feature `{f}` is not a feature of Cargo.toml")
    sym = symbolizer(elf)
    recs, problems = parse_map(elf)
    for p in problems:
        fails.add("NOFP-METADATA", p)
    report = []

    exec_secs = [s for s in elf.sections if s["flags"] & SHF_EXECINSTR and s["flags"] & SHF_ALLOC
                 and s["size"] > 0]
    if not exec_secs:
        fails.add("NOFP-DECODE", "no executable sections")
    if any(s["type"] == SHT_NOBITS for s in exec_secs):
        fails.add("NOFP-DECODE", "an executable section has no file bytes")

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
                    fails.add("NOFP-DECODE", f"executable segment {lo:#x}: unaccounted non-zero "
                              f"bytes {cur:#x}..{a:#x}")
            cur = max(cur, b)
        for s in elf.sections:
            if (s["flags"] & SHF_ALLOC and not s["flags"] & SHF_EXECINSTR and s["addr"] < hi
                    and s["addr"] + s["size"] > lo and s["size"]):
                report.append(f"data section in executable segment (not decoded, not executed as "
                              f"code by design): {s['name']} {s['size']} bytes")

    # Separate the map into candidate regions and exceptions; validate each record's placement.
    live, eliminated, region_recs, exceptions = [], [], [], []
    for r in recs:
        head = r["name"].split(" ", 1)[0]
        known = r["name"] in EXCLUSIONS[arch] or head in POLICY[arch]
        if r["start"] == 0 and r["end"] == 0:
            eliminated.append(r)
            if head in REGION_HEADS:
                region_recs.append(r)
            elif not known:
                fails.add("NOFP-EXCL-UNKNOWN", f"linker-eliminated map record `{r['name']}` names "
                          f"no reviewed {arch} kind")
            continue
        if not r["end"] > r["start"]:
            fails.add("NOFP-METADATA", f"map record {r['name']}: empty or inverted range "
                      f"{r['start']:#x}..{r['end']:#x}")
            continue
        sec = [s for s in exec_secs if s["addr"] <= r["start"] and r["end"] <= s["addr"] + s["size"]]
        if not sec:
            fails.add("NOFP-METADATA", f"map record {r['name']} {r['start']:#x}..{r['end']:#x}: "
                      f"not inside one executable section")
            continue
        r["section"] = sec[0]["name"]
        live.append(r)
        if head in REGION_HEADS:
            region_recs.append(r)
        elif head in POLICY[arch]:
            exceptions.append(r)
        else:
            fails.add("NOFP-EXCL-UNKNOWN", f"map: unknown record kind `{r['name']}` at "
                      f"{r['start']:#x}")
    regions = authenticate(arch, elf, region_recs, feats, fails)
    regions.sort(key=lambda r: r["start"])
    for a, b in zip(regions, regions[1:]):
        if b["start"] < a["end"]:
            fails.add("NOFP-EXCL-OVERLAP", f"regions overlap: {a['name']} and {b['name']}")
    exceptions.sort(key=lambda r: r["start"])
    for a, b in zip(exceptions, exceptions[1:]):
        if b["start"] < a["end"]:
            fails.add("NOFP-EXCL-OVERLAP", f"exception ranges overlap: {a['name']} and {b['name']}")
    for e in exceptions:
        for r in regions:
            if e["start"] < r["end"] and r["start"] < e["end"]:
                if r["mode"] in ("data", "user") or not (r["start"] <= e["start"] and e["end"] <= r["end"]):
                    fails.add("NOFP-EXCL-OVERLAP", f"exception {e['name']} at {e['start']:#x} "
                              f"conflicts with {r['name']} ({r['mode']})")

    # Decode every code byte in its mode.
    native = "native"
    insns = []          # (addr, len, norm, cls, mode)
    data_lines = []     # (addr, len, directive, section-index)
    literal = []        # (target, size)
    acct = dict(code=0, data=0, user=0, sections=0)
    pops = {}
    spans = mapping_spans(elf)
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
            if mode == "data":
                acct["data"] += b - a
                continue
            if mode == "user":
                acct["user"] += b - a
                continue
            dis = disassemble(arch, elf_path, s["name"], a, b, "code64" if mode == "native" else mode)
            pos = a
            for addr, n, text, directive in dis:
                if addr != pos:
                    fails.add("NOFP-DECODE", f"inspection incomplete: {s['name']} {mode}: gap or "
                              f"overlap at {pos:#x} (next decoded {addr:#x}) [{sym(pos)}]")
                    pos = addr
                pos = addr + n
                if directive:
                    if arch == "aarch64" and directive != "inst":
                        data_lines.append((addr, n, directive, s["idx"]))
                        acct["data"] += n
                    else:
                        fails.add("NOFP-DECODE", f"inspection incomplete: the {arch} decoder emitted "
                                  f"`{text}` at {addr:#x} [{sym(addr)}] ({mode}): not decodable code")
                        acct["code"] += n
                    continue
                if "<unknown>" in text or "(bad)" in text or "{bad}" in text or not text:
                    fails.add("NOFP-DECODE", f"inspection incomplete: undecodable bytes in code at "
                              f"{addr:#x} [{sym(addr)}] ({mode})")
                    acct["code"] += n
                    continue
                acct["code"] += n
                norm = normalise(arch, text)
                try:
                    cls = classify(arch, norm, mode)
                except Uninterpretable as why:
                    fails.add("NOFP-DECODE", f"inspection incomplete: cannot interpret `{norm}` at "
                              f"{addr:#x} [{sym(addr)}] ({mode}): {why}")
                    continue
                if arch == "aarch64":
                    t = a64_literal_target(norm)
                    if t:
                        literal.append(t)
                if cls:
                    insns.append((addr, n, norm, cls, mode))
                    key = f"{cls}:{split(norm, arch, mode)[0]}"
                    pops[key] = pops.get(key, 0) + 1
            if pos != b:
                fails.add("NOFP-DECODE", f"inspection incomplete: {s['name']} {mode}: decoded to "
                          f"{pos:#x}, region ends {b:#x} [{sym(pos)}]")

    # Data the decoder emitted inside decoded code: only AArch64 literal-pool words, i.e. inside a
    # `$d` mapping span and covered by the target of a PC-relative literal load.
    literal.sort()
    for addr, n, directive, shndx in data_lines:
        covered = all(any(t <= x < t + sz for t, sz in literal) for x in range(addr, addr + n))
        if not in_data_span(spans, shndx, addr, n):
            fails.add("NOFP-DATA", f"`.{directive}` at {addr:#x} [{sym(addr)}] is outside every "
                      f"`$d` mapping span")
        elif not covered:
            fails.add("NOFP-DATA", f"`.{directive}` at {addr:#x} [{sym(addr)}] is not the target of "
                      f"any PC-relative literal load in decoded code")

    # Every classified instruction belongs to exactly one exception range.
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
        fails.add("NOFP-ORDINARY", f"FORBIDDEN {cls} instruction in ordinary code at {addr:#x} "
                  f"[{sym(addr)}]: {norm}")

    # Each exception range: exactly its kind's ordered pattern, bounded size, instance counts.
    counts = {}
    for e in exceptions:
        kind = e["name"].split(" ", 1)[0]
        purpose, pats, max_insns, _ = POLICY[arch][kind]
        counts[kind] = counts.get(kind, 0) + 1
        got = inside[id(e)]
        where = f"{kind} at {e['start']:#x} [{sym(e['start'])}]"
        if len(got) != len(pats):
            fails.add("NOFP-EXCEPTION", f"exception {where}: {len(got)} FP/control instructions, "
                      f"policy allows exactly {len(pats)}: {[g[1] for g in got]}")
        else:
            for (addr, norm, cls), pat in zip(got, pats):
                if not re.fullmatch(pat, norm):
                    fails.add("NOFP-EXCEPTION", f"exception {where}: instruction at {addr:#x} "
                              f"`{norm}` is not the permitted `{pat}`")
        if e["end"] - e["start"] > max(max_insns, len(pats)) * 16 + 128:
            fails.add("NOFP-EXCEPTION", f"exception {where}: range of {e['end'] - e['start']} "
                      f"bytes exceeds its bound")
    for kind, (purpose, pats, mx, expected) in POLICY[arch].items():
        want = expected(feats)
        have = counts.get(kind, 0)
        if have != want:
            fails.add("NOFP-INVENTORY", f"exception inventory: {kind} has {have} instance(s), this "
                      f"build (features {sorted(feats) or ['<none>']}) requires {want}")

    if boot_image:
        for f in check_boot_image(elf, boot_image):
            fails.add("NOFP-BOOT-IMAGE", f)

    return dict(fails=fails, report=report, acct=acct, pops=pops, exceptions=exceptions,
                eliminated=eliminated, regions=regions, ordinary=ordinary, insns=insns,
                feats=feats, sym=sym, data_lines=data_lines)


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
    # Prefix grammar: every REX form, stacked prefixes, REX2, pseudo-prefixes, branch hints.
    ("x86_64", "rex.WB fninit", "fp"), ("x86_64", "rex fninit", "fp"), ("x86_64", "rex.R fninit", "fp"),
    ("x86_64", "rex.WRXB fninit", "fp"), ("x86_64", "rex.RXB fninit", "fp"),
    ("x86_64", "{rex2 0x0} fninit", "fp"), ("x86_64", "data16 rex fninit", "fp"),
    ("x86_64", "fs rex.WB fninit", "fp"), ("x86_64", "repz rex.WB fninit", "fp"),
    ("x86_64", "repnz repz fninit", "fp"), ("x86_64", "rex.W ldmxcsr (%rax)", "fp"),
    ("x86_64", "rex.WR ldmxcsr (%rax)", "fp"), ("x86_64", "rex.B emms", "fp"),
    ("x86_64", "rex.B vzeroupper", "fp"), ("x86_64", "rex stmxcsr (%rax)", "fp"),
    ("x86_64", "rex.WB fnsave (%r8)", "fp"), ("x86_64", "rex.WB finit", "fp"),
    ("x86_64", "rex.WB mov %r8,%cr0", "enable"), ("x86_64", "rex clts", "enable"),
    ("x86_64", "rex.W xsetbv", "enable"), ("x86_64", "{vex} vpdpbusd %xmm1,%xmm0,%xmm0", "fp"),
    ("x86_64", "lock add %rax,(%rcx)", None), ("x86_64", "rep stos %rax,%es:(%rdi)", None),
    ("x86_64", "data16 cs nopw 0x0(%rax,%rax,1)", None), ("x86_64", "notrack jmp *%rax", None),
    ("x86_64", "bnd jmp *%rax", None), ("x86_64", "xacquire lock add %rax,(%rcx)", None),
    ("x86_64", "repz ret", None), ("x86_64", "rex nop", None), ("x86_64", "rex.W", None),
    ("x86_64", "je,pt 0x3", None), ("x86_64", "{rex2 0x0} nop", None),
    ("x86_64", "addr32 call 0x6", None), ("x86_64", "cs ds nop", None),
    ("x86_64", "fwait", "fp"), ("x86_64", "tilerelease", "fp"),
    ("aarch64", "stp q0,q1,[sp,#288]", "fp"), ("aarch64", "fmov d0,x1", "fp"),
    ("aarch64", "ldr q0,[x1]", "fp"), ("aarch64", "movi v0.2d,#0", "fp"),
    ("aarch64", "mrs x9,fpcr", "fp"), ("aarch64", "msr fpsr,xzr", "fp"),
    ("aarch64", "msr cpacr_el1,x1", "enable"), ("aarch64", "mrs x8,cpacr_el1", None),
    ("aarch64", "ldp x0,x1,[sp]", None), ("aarch64", "b 0x40", None), ("aarch64", "add x0,x1,#8", None),
    ("aarch64", "str s0,[x0]", "fp"), ("aarch64", "msr tpidr_el0,x9", None),
    ("aarch64", "cnt v0.8b,v0.8b", "fp"), ("aarch64", "ucvtf d0,x0", "fp"),
    ("aarch64", "b.ne 0x40", None), ("aarch64", "add x0,x1,x2,lsl #3", None),
    ("riscv64", "fld ft0,0(a0)", "fp"), ("riscv64", "fmv.x.d a0,fa0", "fp"),
    ("riscv64", "csrr a0,fcsr", "fp"), ("riscv64", "frcsr a0", "fp"), ("riscv64", "fence rw,rw", None),
    ("riscv64", "fence.i", None), ("riscv64", "vsetvli t0,a0,e8,m1,ta,ma", "fp"),
    ("riscv64", "csrrc a0,sstatus,a1", None), ("riscv64", "ld s0,0(sp)", None),
    ("riscv64", "add a0,a1,a2", None), ("riscv64", "csrw vstart,zero", "fp"),
]

# Text the checker must refuse to interpret (each would otherwise hide what follows it).
SELF_TEST_UNINTERPRETABLE = [
    ("x86_64", "rex.q fninit", "code64"), ("x86_64", "rex.bw fninit", "code64"),
    ("x86_64", "{bad} fninit", "code64"), ("x86_64", "{rex2} fninit", "code64"),
    ("x86_64", "xyzzy lock add %rax,(%rcx)", "code64"), ("x86_64", "fwait lock fninit", "code64"),
    ("x86_64", "rex.wb fninit", "code32"), ("x86_64", "data32 fninit", "code64"),
    ("x86_64", "addr16 fninit", "code16"), ("x86_64", "mov %rax,%rbx junk", "code64"),
    ("x86_64", "0xdead", "code64"), ("aarch64", "!! x0", "native"), ("riscv64", "!x a0", "native"),
    ("x86_64", "addr32 fninit", "code32"), ("x86_64", "xyzzy fninit", "code64"),
]


def self_test():
    bad = 0
    for arch, text, want in SELF_TEST:
        try:
            got = classify(arch, normalise(arch, text))
        except Uninterpretable as e:
            got = f"uninterpretable ({e})"
        if got != want:
            bad += 1
            print(f"MISMATCH {arch} `{text}`: got {got}, want {want}")
    for arch, text, mode in SELF_TEST_UNINTERPRETABLE:
        try:
            got = classify(arch, normalise(arch, text), mode)
            bad += 1
            print(f"MISMATCH {arch} `{text}` ({mode}): interpreted as {got}, want uninterpretable")
        except Uninterpretable:
            pass
    # exactly the decoder's 16 REX spellings are prefixes
    rex = [f"rex{'.' + s if s else ''}" for s in
           ("", "b", "x", "xb", "r", "rb", "rx", "rxb", "w", "wb", "wx", "wxb", "wr", "wrb", "wrx", "wrxb")]
    if sorted(t for t in rex if X86_REX.fullmatch(t)) != sorted(rex) or X86_REX.fullmatch("rex.") \
            or X86_REX.fullmatch("rex.bw"):
        bad += 1
        print("MISMATCH REX grammar")
    # every policy pattern must accept the instruction it was written for
    samples = {"X86_TRAP_ENTRY_CAPTURE": ["fxsave64 (%rsp)", "fninit",
                                          "ldmxcsr yarm_x86_kernel_mxcsr(%rip)"]}
    for kind, insns in samples.items():
        for pat, t in zip(POLICY["x86_64"][kind][1], insns):
            if not re.fullmatch(pat, t):
                bad += 1
                print(f"MISMATCH policy {kind}: `{t}` vs `{pat}`")
    # the inventory is closed and consistent: names parse, features exist, anchors are distinct
    graph = cargo_features(os.path.join(REPO, "Cargo.toml"))
    for arch, inv in EXCLUSIONS.items():
        for name, ent in inv.items():
            head = name.split(" ", 1)[0]
            ok = (RECORD_NAME.fullmatch(name) and head in REGION_HEADS
                  and (ent["feature"] is None or ent["feature"] in graph)
                  and os.path.isfile(os.path.join(REPO, ent["source"]))
                  and {"CODE16": "code16", "CODE32": "code32", "DATA": "data",
                       "USER_PROGRAM": "user"}[head] == ent["mode"]
                  and (arch == "x86_64" or ent["mode"] == "user"))
            if not ok:
                bad += 1
                print(f"MISMATCH inventory {arch} `{name}`")
    for arch, pol in POLICY.items():
        for kind, (_, _, _, expected) in pol.items():
            for f in ("context1-witness", "context1-clobber"):
                if f not in graph:
                    bad += 1
                    print(f"MISMATCH policy feature `{f}` not in Cargo.toml")
    n = len(SELF_TEST) + len(SELF_TEST_UNINTERPRETABLE)
    print(f"NOFP_SELF_TEST cases={n} mismatches={bad} result={'ok' if bad == 0 else 'fail'}")
    return 1 if bad else 0


def check_tools(arches):
    """`--check-tools`: one NOFP_TOOLS line per arch; exit 1 on the first unusable tool."""
    bad = 0
    for arch in arches:
        try:
            rec = require_tool(arch)
            print(f"NOFP_TOOLS arch={arch} tool={rec['name']} path={rec['path']} "
                  f"selected_by={rec['selected_by']} version=\"{rec['version']}\" "
                  f"probe=ok result=ok")
        except ToolError as e:
            bad = 1
            print(f"  FAIL [NOFP-TOOLS] {e}")
            print(f"NOFP_TOOLS arch={arch} tool={TOOL_FOR_ARCH[arch]} result=fail")
    return bad


def list_inventory():
    for arch, inv in EXCLUSIONS.items():
        for name, ent in inv.items():
            segs = ",".join(f"{a if isinstance(a, str) else a[0] + ':' + a[1]}.."
                            f"{b if isinstance(b, str) else b[0] + ':' + b[1]}"
                            for a, b in ent["segments"])
            print(f"{arch}\t{name}\t{ent['mode']}\t{ent['source']}\t{ent['section']}\t"
                  f"{ent['feature'] or '-'}\t{segs}")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", choices=["x86_64", "aarch64", "riscv64"])
    ap.add_argument("--elf")
    ap.add_argument("--features", default="")
    ap.add_argument("--toolchain", default="unrecorded")
    ap.add_argument("--boot-image")
    ap.add_argument("--census", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--list-inventory", action="store_true")
    ap.add_argument("--check-tools", action="store_true",
                    help="resolve, version-gate and probe the disassemblers (all arches, or --arch)")
    ap.add_argument("--max-lines", type=int, default=40)
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if a.list_inventory:
        return list_inventory()
    if a.check_tools:
        return check_tools([a.arch] if a.arch else ["x86_64", "aarch64", "riscv64"])
    if not a.arch or not a.elf:
        ap.error("--arch and --elf are required")
    if not os.path.isfile(a.elf):
        print(f"NOFP_CHECK arch={a.arch} image={a.elf} obligations=NOFP-DECODE result=fail "
              f"reason=inspection_incomplete:image_missing")
        return 1
    with open(a.elf, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    try:
        res = check(a.arch, a.elf, a.features, a.census, a.boot_image)
        trec = _TOOLS[TOOL_FOR_ARCH[a.arch]]
        dis = f"{trec['version']}\" disassembler_path=\"{trec['path']}"
    except ToolError as e:
        print(f"  FAIL [NOFP-TOOLS] {e}")
        print(f"NOFP_CHECK arch={a.arch} image_sha256={digest} obligations=NOFP-TOOLS result=fail "
              f"reason=tools")
        return 1
    except (SystemExit, Exception) as e:  # an inspection that could not finish is a failure
        why = str(e).replace("\n", " ")[:200] or type(e).__name__
        print(f"  FAIL [NOFP-DECODE] inspection incomplete: {why}")
        print(f"NOFP_CHECK arch={a.arch} image_sha256={digest} obligations=NOFP-DECODE result=fail "
              f"reason=inspection_incomplete")
        return 1
    sym = res["sym"]
    for e in res["exceptions"]:
        kind = e["name"].split(" ", 1)[0]
        print(f"  exception {kind:<24} {e['start']:#x}..{e['end']:#x} ({e['end'] - e['start']} bytes) "
              f"in {sym(e['start'])}")
    for r in res["regions"]:
        ent = r["entry"]
        print(f"  exclusion {r['name']:<28} {r['start']:#x}..{r['end']:#x} ({r['end'] - r['start']} "
              f"bytes) {ent['mode']} anchored owner={ent['source']}")
    for r in res["eliminated"]:
        print(f"  eliminated-by-linker map record: {r['name']}")
    for addr, n, directive, _ in res["data_lines"]:
        print(f"  literal-pool data .{directive} {addr:#x} ({n} bytes) [{sym(addr)}]")
    for line in res["report"]:
        print(f"  {line}")
    acct = res["acct"]
    print(f"  accounted: executable={acct['sections']} decoded_code={acct['code']} "
          f"data={acct['data']} user_program={acct['user']} "
          f"unaccounted={acct['sections'] - acct['code'] - acct['data'] - acct['user']}")
    if acct["sections"] != acct["code"] + acct["data"] + acct["user"]:
        res["fails"].add("NOFP-DECODE", "inspection incomplete: executable bytes not fully accounted")
    pops = res["pops"]
    total = sum(pops.values())
    ordinary = len(res["ordinary"])
    print(f"  population: classified={total} in_exceptions={total - ordinary} ordinary={ordinary}")
    for k in sorted(pops, key=lambda k: -pops[k])[:12]:
        print(f"    {pops[k]:>7} {k}")
    shown = 0
    for code, f in res["fails"]:
        if shown < a.max_lines:
            print(f"  FAIL [{code}] {f}")
        shown += 1
    if shown > a.max_lines:
        print(f"  ... {shown - a.max_lines} more failure line(s)")
    codes = ",".join(sorted({c for c, _ in res["fails"]})) or "-"
    verdict = "census" if a.census else ("pass" if not res["fails"] else "fail")
    print(f"NOFP_CHECK arch={a.arch} image_sha256={digest} features={','.join(sorted(res['feats'])) or '-'} "
          f"toolchain=\"{a.toolchain}\" disassembler=\"{dis}\" failures={len(res['fails'])} "
          f"ordinary_fp={ordinary} obligations={codes} result={verdict}")
    return 0 if verdict == "pass" else (0 if a.census else 1)


if __name__ == "__main__":
    sys.exit(main())
