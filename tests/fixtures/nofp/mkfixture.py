#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KERNEL-NOFP1-ACCEPTANCE — synthetic final-image fixtures for `scripts/check-kernel-nofp.py`.

    mkfixture.py --out DIR      write DIR/<case>.elf for every case and DIR/cases.tsv
                                (`case<TAB>arch<TAB>features<TAB>stub`, one line per case;
                                `-` for no features / no disassembler stub)
    mkfixture.py --list         print the case names

Each fixture is a small linked-looking ELF64 image written byte by byte (no assembler needed): the
port's exec sections, its symbol table (function extents, the global anchor symbols the real
sources define, AArch64/RISC-V mapping symbols) and a `.yarm_nofp_map` section in exactly the
format `nofp_map_record!` emits. The baseline image of each port carries the port's whole
audited exception inventory with the real instruction sequences, the x86 boot-mode regions, and
— when the case names a witness feature — the embedded user program, so a case differs from its
baseline only by the defect it exercises.

These are verifier fixtures on synthetic images: they test what the checker accepts and
rejects. They say nothing about the delivered kernel images, which are checked as built.
"""

import argparse
import os
import struct
import sys

# ── ELF writer ─────────────────────────────────────────────────────────────────────────────────

EM = {"x86_64": 62, "aarch64": 183, "riscv64": 243}
EFLAGS = {"x86_64": 0, "aarch64": 0, "riscv64": 0x5}  # RISC-V: RVC | double-float ABI
SHT_PROGBITS, SHT_SYMTAB, SHT_STRTAB, SHT_NOBITS = 1, 2, 3, 8
SHF_WRITE, SHF_ALLOC, SHF_EXEC = 1, 2, 4
STT = {"notype": 0, "object": 1, "func": 2}
STB = {"local": 0, "global": 1}
MAGIC = 0x3150464F4E4D5259


class Section:
    def __init__(self, name, addr, flags, align=16):
        self.name, self.addr, self.flags, self.align = name, addr, flags, align
        self.buf = bytearray()

    def here(self):
        return self.addr + len(self.buf)


class Image:
    def __init__(self, arch):
        self.arch = arch
        self.sections = []
        self.symbols = []      # (name, value, size, type, bind, section-name or None)
        self.records = []      # (start, end, name) in emission order
        self.map_tail = b""    # raw bytes appended to the map section (malformed-map fixtures)
        self.cur = None

    def section(self, name, addr, flags, align=16):
        s = Section(name, addr, flags, align)
        self.sections.append(s)
        self.cur = s
        return s

    def use(self, name):
        self.cur = next(s for s in self.sections if s.name == name)

    def here(self):
        return self.cur.here()

    def emit(self, data):
        if isinstance(data, str):
            data = bytes.fromhex(data.replace(" ", ""))
        self.cur.buf += data

    def word(self, w):
        self.emit(struct.pack("<I", w & 0xFFFFFFFF))

    def align(self, n, fill=b"\0"):
        while len(self.cur.buf) % n:
            self.cur.buf += fill

    def sym(self, name, value=None, size=0, typ="notype", bind="local", section=None):
        self.symbols.append((name, self.here() if value is None else value, size, typ, bind,
                             self.cur.name if section is None else section))

    def record(self, start, end, name):
        self.records.append((start, end, name))

    def func(self, name, body, bind="global"):
        """A sized STT_FUNC symbol around `body(self)`; returns (start, end)."""
        start = self.here()
        if self.arch != "x86_64":
            self.sym("$x", start)
        body(self)
        end = self.here()
        self.symbols.append((name, start, end - start, "func", bind, self.cur.name))
        return start, end

    def write(self, path):
        secs = [s for s in self.sections]
        mapsec = bytearray()
        for start, end, name in self.records:
            while len(mapsec) % 8:
                mapsec += b"\0"
            mapsec += struct.pack("<QQQ", MAGIC, start, end) + name.encode() + b"\0"
        while len(mapsec) % 8:
            mapsec += b"\0"
        mapsec += self.map_tail

        shstr = bytearray(b"\0")

        def shname(n):
            off = len(shstr)
            shstr.extend(n.encode() + b"\0")
            return off

        strtab = bytearray(b"\0")

        def strname(n):
            off = len(strtab)
            strtab.extend(n.encode() + b"\0")
            return off

        sec_index = {s.name: i + 1 for i, s in enumerate(secs)}
        syms = sorted(self.symbols, key=lambda s: (STB[s[4]], s[1]))
        first_global = 1 + sum(1 for s in syms if s[4] == "local")
        symtab = bytearray(b"\0" * 24)
        for name, value, size, typ, bind, sec in syms:
            shndx = sec_index[sec] if sec else 0xFFF1
            symtab += struct.pack("<IBBHQQ", strname(name), (STB[bind] << 4) | STT[typ], 0, shndx,
                                  value, size)

        loads = [s for s in secs if s.flags & SHF_ALLOC]
        nph = len(loads)
        off = 64 + 56 * nph
        layout = []
        for s in secs:
            off = (off + 0xFFF) & ~0xFFF
            layout.append(off)
            off += len(s.buf)
        extra = []
        for name, data, typ, link, info, entsize, align in (
                (".yarm_nofp_map", bytes(mapsec), SHT_PROGBITS, 0, 0, 0, 8),
                (".symtab", bytes(symtab), SHT_SYMTAB, len(secs) + 4, first_global, 24, 8),
                (".shstrtab", None, SHT_STRTAB, 0, 0, 0, 1),
                (".strtab", bytes(strtab), SHT_STRTAB, 0, 0, 0, 1)):
            extra.append([name, data, typ, link, info, entsize, align])
        # section names
        names = [shname(s.name) for s in secs] + [shname(e[0]) for e in extra]
        extra[2][1] = bytes(shstr)
        eoffs = []
        for e in extra:
            off = (off + e[6] - 1) & ~(e[6] - 1)
            eoffs.append(off)
            off += len(e[1])
        shoff = (off + 7) & ~7
        nsh = 1 + len(secs) + len(extra)
        shstrndx = 1 + len(secs) + 2

        out = bytearray(shoff + 64 * nsh)
        entry = secs[0].addr
        out[0:64] = (b"\x7fELF" + bytes([2, 1, 1, 0]) + b"\0" * 8 + struct.pack(
            "<HHIQQQIHHHHHH", 2, EM[self.arch], 1, entry, 64, shoff, EFLAGS[self.arch], 64, 56,
            nph, 64, nsh, shstrndx))
        for i, s in enumerate(loads):
            o = layout[secs.index(s)]
            flags = 4 | (1 if s.flags & SHF_EXEC else 0) | (2 if s.flags & SHF_WRITE else 0)
            out[64 + 56 * i:64 + 56 * (i + 1)] = struct.pack(
                "<IIQQQQQQ", 1, flags, o, s.addr, s.addr & 0xFFFFFFFF, len(s.buf), len(s.buf),
                0x1000)
        for s, o in zip(secs, layout):
            out[o:o + len(s.buf)] = s.buf
        for e, o in zip(extra, eoffs):
            out[o:o + len(e[1])] = e[1]
        sh = shoff + 64
        for i, s in enumerate(secs):
            out[sh:sh + 64] = struct.pack("<IIQQQQIIQQ", names[i], SHT_PROGBITS, s.flags, s.addr,
                                          layout[i], len(s.buf), 0, 0, s.align, 0)
            sh += 64
        for j, (e, o) in enumerate(zip(extra, eoffs)):
            out[sh:sh + 64] = struct.pack("<IIQQQQIIQQ", names[len(secs) + j], e[2], 0, 0, o,
                                          len(e[1]), e[3], e[4], e[6], e[5])
            sh += 64
        with open(path, "wb") as f:
            f.write(out)


# ── x86_64 ─────────────────────────────────────────────────────────────────────────────────────

X86_TEXT = 0xFFFFFFFF81000000
X86_RODATA = 0xFFFFFFFF81100000
X86_MXCSR = X86_RODATA
X86_INITIAL_FXSAVE = X86_RODATA + 0x40


def rel32(img, insn_len, target):
    return struct.pack("<i", target - (img.here() + insn_len))


def exception(img, kind, body):
    """`body` emitted between two labels recorded as exception `kind`."""
    a = img.here()
    body(img)
    img.record(a, img.here(), kind)


def emit_fn(x):
    """An option value: hex text, bytes, or a callable taking the image."""
    if x is None:
        return lambda i: None
    if callable(x):
        return x
    return lambda i: i.emit(x)


def x86_image(o):
    img = Image("x86_64")
    # .text.boot32: the PVH 32-bit entry, one CODE32 region over the whole section.
    img.section(".text.boot32", o.get("boot32_addr", 0x100020), SHF_ALLOC | SHF_EXEC)
    b32 = img.here()
    img.sym("pvh_start32", typ="func", bind="global")
    img.emit("fa")                                   # cli
    img.emit("0f 20 e0 83 c8 20")                     # mov %cr4,%eax ; or $0x20,%eax
    exception(img, "X86_CR4_WRITE", lambda i: i.emit("0f 22 e0"))
    img.emit("0f 20 c0 0d 01 00 00 80")               # mov %cr0,%eax ; or $0x80000001,%eax
    exception(img, "X86_CR0_WRITE", lambda i: i.emit("0f 22 c0"))
    img.emit("eb fe")                                 # jmp .
    img.sym("uart_putc32")
    img.emit("c3")
    img.record(b32, img.here(), "CODE32 boot32")

    img.section(".text", X86_TEXT, SHF_ALLOC | SHF_EXEC)

    def kernel_main(i):
        i.emit("55 48 89 e5")                         # push %rbp ; mov %rsp,%rbp
        emit_fn(o.get("ordinary"))(i)
        i.emit("48 89 c3 5d c3")                      # mov %rax,%rbx ; pop %rbp ; ret
    img.func("kernel_main", kernel_main)

    for n in range(2):
        def capture(i):
            i.emit("48 81 ec 00 02 00 00")            # sub $0x200,%rsp

            def rng(i):
                i.emit("48 0f ae 04 24")              # fxsave64 (%rsp)
                i.emit("48 89 e7")                    # mov %rsp,%rdi (an integer insn inside)
                i.emit("db e3")                       # fninit
                i.emit("0f ae 15")                    # ldmxcsr yarm_x86_kernel_mxcsr(%rip)
                i.emit(rel32(i, 4, X86_MXCSR))
            exception(i, "X86_TRAP_ENTRY_CAPTURE", rng)
            i.emit("e8 00 00 00 00")                  # call (next)
            exception(i, "X86_TRAP_ENTRY_RESTORE", lambda i: i.emit("48 0f ae 0c 24"))
            i.emit("48 81 c4 00 02 00 00 48 cf")      # add $0x200,%rsp ; iretq
        img.func(f"trap_entry_{n}", capture)

    def first_entry(i):
        def rng(i):
            i.emit("48 0f ae 0d")                     # fxrstor64 yarm_x86_initial_fxsave(%rip)
            i.emit(rel32(i, 4, X86_INITIAL_FXSAVE))
        exception(i, "X86_FIRST_ENTRY_RESTORE", rng)
        i.emit("48 0f 07")                            # sysretq
    img.func("enter_ring3_first", first_entry)

    def resume(i):
        exception(i, "X86_RESUME_RESTORE", lambda i: i.emit("48 0f ae 0e"))
        i.emit("48 cf")
    img.func("resume_ring3", resume)

    def switch(i):
        exception(i, "X86_SWITCH_SAVE", lambda i: i.emit("0f ae 47 40"))
        i.emit("48 89 67 38 48 8b 66 38")             # mov %rsp,0x38(%rdi) ; mov 0x38(%rsi),%rsp
        exception(i, "X86_SWITCH_RESTORE", lambda i: i.emit("0f ae 4e 40"))
        i.emit("c3")
    img.func("yarm_x86_switch_frame", switch)

    def frame_init(i):
        exception(i, "X86_SWITCH_FRAME_INIT", lambda i: i.emit("db e3 0f ae 07"))
        i.emit("c3")                                  # fninit ; fxsave (%rdi)
    img.func("initialize_frame_fpu_state", frame_init)

    def fp_env(i):
        i.emit("0f 20 c0 48 83 e0 f3 48 83 c8 22")   # mov %cr0,%rax ; and ; or
        exception(i, "X86_CR0_WRITE", lambda i: i.emit("0f 22 c0"))
        i.emit("0f 20 e0 48 0d 00 06 00 00")          # mov %cr4,%rax ; or $0x600,%rax
        exception(i, "X86_CR4_WRITE", lambda i: i.emit("0f 22 e0"))

        def env(i):
            i.emit("db e3 0f ae 15")
            i.emit(rel32(i, 4, X86_MXCSR))
        exception(i, "X86_KERNEL_FP_ENV_INIT", env)
        i.emit("c3")
    img.func("establish_fpu_control_on_this_cpu", fp_env)

    def cr4_ap(i):
        i.emit("0f 20 e0 48 0d 80 00 00 00")
        exception(i, "X86_CR4_WRITE", lambda i: i.emit("0f 22 e0"))
        i.emit("c3")
    img.func("ap_enable_pge", cr4_ap)

    # The AP trampoline blob (copied to one low page): CODE16, GDT data, CODE32, long-mode code,
    # handoff data.
    img.align(64, b"\xcc")
    t0 = img.here()
    img.sym("yarm_ap_trampoline_start", bind="global")
    img.emit("fa fc 31 c0 8e d8")                     # cli ; cld ; xor %ax,%ax ; mov %ax,%ds
    img.emit("0f 20 c0 66 83 c8 01")                  # mov %cr0,%eax ; or $1,%eax
    exception(img, "X86_CR0_WRITE", lambda i: i.emit("0f 22 c0"))
    img.emit("66 ea 28 70 00 00 08 00")               # ljmpl $0x8,$0x7028
    gdtr = img.here()
    img.sym("ap_gdtr")
    img.emit("27 00 00 70 00 00")
    img.align(8)
    img.sym("ap_gdt")
    img.emit("00" * 8 + "ffff0000009acf00 ffff00000092cf00 ffff0000009aaf00 ffff00000092af00")
    pm = img.here()
    img.sym("ap_protected_entry")
    img.emit("66 b8 10 00 8e d8")                     # mov $0x10,%ax ; mov %eax,%ds
    img.emit("0f 20 e0 83 c8 20")
    exception(img, "X86_CR4_WRITE", lambda i: i.emit("0f 22 e0"))
    img.emit("0f 20 c0 0d 00 00 00 80")
    exception(img, "X86_CR0_WRITE", lambda i: i.emit("0f 22 c0"))
    emit_fn(o.get("code32_pad"))(img)
    img.emit("ea 00 71 00 00 18 00")                  # ljmp $0x18,$0x7100
    lm = img.here()
    img.sym("ap_long_entry")
    img.emit("48 8b 25 00 00 00 00 ff e0")            # mov 0x0(%rip),%rsp ; jmp *%rax
    img.align(64, b"\xcc")
    ho = img.here()
    img.sym("yarm_ap_trampoline_handoff", bind="global")
    emit_fn(o.get("handoff", bytes(range(1, 9)) * 17))(img)
    he = img.here()
    img.sym("yarm_ap_trampoline_end", bind="global")
    img.record(t0, gdtr, "CODE16 ap_trampoline")
    img.record(gdtr, pm, "DATA ap_trampoline_gdt")
    img.record(pm, lm, "CODE32 ap_trampoline")
    img.record(ho, he, "DATA ap_trampoline_handoff")
    img.func("after_trampoline", emit_fn(o.get("after_trampoline", "c3")))
    emit_fn(o.get("user"))(img)
    img.func("after_user", emit_fn(o.get("after_user", "c3")))
    emit_fn(o.get("text_end"))(img)

    img.section(".rodata", X86_RODATA, SHF_ALLOC)
    img.sym("yarm_x86_kernel_mxcsr", size=4, typ="object", bind="global")
    img.emit(struct.pack("<I", 0x1F80) + b"\0" * 0x3C)
    img.sym("yarm_x86_initial_fxsave", size=512, typ="object", bind="global")
    img.emit(b"\0" * 512)
    if "records" in o:
        img.records = o["records"](img.records)
    img.map_tail = o.get("map_tail", b"")
    if "symbols" in o:
        img.symbols = o["symbols"](img.symbols)
    return img


def program(img, segments, body, record=None, align=16, fill=b"\xcc"):
    """An embedded user program: consecutive segments, each between its global start/end anchor
    symbols, with `body(img, k)` as segment k's bytes. Records it as `record` unless None.
    Returns (start, end)."""
    img.align(align, fill)
    s = img.here()
    for k, (a, b) in enumerate(segments):
        img.sym(a, bind="global")
        body(img, k)
        img.sym(b, bind="global")
    e = img.here()
    if record:
        img.record(s, e, record)
    return s, e


def x86_vmi2(record="USER_PROGRAM vmi2_witness"):
    def body(i, k):
        i.emit("0f 28 c8 db e3 0f ae 1c 24")          # movaps %xmm0,%xmm1 ; fninit ; stmxcsr
        i.emit("b8 0f 00 00 00 0f 05 eb fe")          # mov $15,%eax ; syscall ; jmp .
        i.sym("vmi2_msg")
        i.emit(b"VMI2_T1_POST")
    return lambda img: program(img, [("yarm_vmi2_start", "yarm_vmi2_end")], body, record)


def x86_overtaken(gap=None):
    def body(i, k):
        i.emit("66 0f ef c0 0f 05")                   # pxor %xmm0,%xmm0 ; syscall
        i.emit(b"OVT-W" if k == 0 else b"OVT-K")
    segs = [("yarm_ovt_x86_w_start", "yarm_ovt_x86_w_end"),
            ("yarm_ovt_x86_k_start", "yarm_ovt_x86_k_end")]
    if gap is None:
        return lambda img: program(img, segs, body, "USER_PROGRAM overtaken_witness")

    def split_prog(img):
        img.align(16, b"\xcc")
        s = img.here()
        img.sym(segs[0][0], bind="global")
        body(img, 0)
        img.sym(segs[0][1], bind="global")
        img.emit(gap)                                 # kernel bytes between the two segments
        img.sym(segs[1][0], bind="global")
        body(img, 1)
        img.sym(segs[1][1], bind="global")
        img.record(s, img.here(), "USER_PROGRAM overtaken_witness")
    return split_prog


# ── AArch64 ────────────────────────────────────────────────────────────────────────────────────

A64_BOOT = 0x40080000
A64_TEXT = 0x40081000
A64_SYSREG = {"fpcr": (3, 3, 4, 4, 0), "fpsr": (3, 3, 4, 4, 1), "cpacr_el1": (3, 0, 1, 0, 2),
              "cptr_el2": (3, 4, 1, 1, 2)}


def a64_sys(base, reg, t):
    op0, op1, crn, crm, op2 = A64_SYSREG[reg]
    return base | ((op0 & 1) << 19) | (op1 << 16) | (crn << 12) | (crm << 8) | (op2 << 5) | t


def mrs(t, reg):
    return a64_sys(0xD5300000, reg, t)


def msr(reg, t):
    return a64_sys(0xD5100000, reg, t)


def stp_q(t, t2, n, off):
    return 0xAD000000 | (((off // 16) & 0x7F) << 15) | (t2 << 10) | (n << 5) | t


def ldp_q(t, t2, n, off):
    return 0xAD400000 | (((off // 16) & 0x7F) << 15) | (t2 << 10) | (n << 5) | t


A64 = dict(ret=0xD65F03C0, nop=0xD503201F, eret=0xD69F03E0, isb=0xD5033FDF, svc=0xD4000001,
           fmov_d0_x1=0x9E670020, mov_x1_cpacr=0xD2A00601, add_x0_x1_8=0x91002020,
           str_x9=0xF90193E9, ldr_x9=0xF94193E9, xzr=31)


def words(*ws):
    def f(i):
        for w in ws:
            i.word(w)
    return f


def a64_image(o):
    img = Image("aarch64")
    img.section(".text.boot", A64_BOOT, SHF_ALLOC | SHF_EXEC)

    def start(i):
        i.word(A64["mov_x1_cpacr"])
        exception(i, "A64_CPACR_WRITE", words(msr("cpacr_el1", 1)))
        i.word(A64["isb"])
        exception(i, "A64_CPTR_EL2_WRITE", words(msr("cptr_el2", 31)))
        i.word(0x58000041)                            # ldr x1, .+8   (the literal below)
        i.word(0x14000000 | (4 + o.get("pool_extra", 0)) // 4 + 2)  # b over the pool
        i.sym("$d")
        i.emit(struct.pack("<Q", 0x30D00800))         # the literal-pool doubleword
        i.emit(b"\0" * o.get("pool_extra", 0))
        i.sym("$x")
        i.word(A64["ret"])
    img.func("_start", start)

    img.section(".text", A64_TEXT, SHF_ALLOC | SHF_EXEC)

    def kernel_main(i):
        i.word(A64["nop"])
        emit_fn(o.get("ordinary"))(i)
        i.word(A64["add_x0_x1_8"])
        i.word(A64["ret"])
    img.func("kernel_main", kernel_main)

    def vectors(i):
        exception(i, "A64_VECTOR_CAPTURE", words(
            *[stp_q(q, q + 1, 31, 288 + 16 * q) for q in range(0, 32, 2)],
            mrs(9, "fpcr"), A64["str_x9"], mrs(9, "fpsr"), A64["str_x9"],
            msr("fpcr", 31)))
        i.word(A64["nop"])
        exception(i, "A64_VECTOR_RESTORE", words(
            *[ldp_q(q, q + 1, 31, 288 + 16 * q) for q in range(0, 32, 2)],
            A64["ldr_x9"], msr("fpcr", 9), A64["ldr_x9"], msr("fpsr", 9)))
        i.word(A64["eret"])
    img.func("yarm_aarch64_vectors", vectors)

    def first_entry(i):
        exception(i, "A64_FIRST_ENTRY_INIT", words(
            *[0x6F00E400 | v for v in range(32)], msr("fpcr", 31), msr("fpsr", 31)))
        i.word(A64["eret"])
    img.func("enter_el0_first", first_entry)

    def switch(i):
        exception(i, "A64_SWITCH_SAVE", words(
            *[stp_q(q, q + 1, 0, 128 + 16 * (q - 8)) for q in range(8, 16, 2)]))
        i.word(A64["nop"])
        exception(i, "A64_SWITCH_RESTORE", words(
            *[ldp_q(q, q + 1, 1, 128 + 16 * (q - 8)) for q in range(8, 16, 2)]))
        i.word(A64["ret"])
    img.func("yarm_aarch64_switch_frame", switch)

    for n in range(2):
        def cpacr(i):
            i.word(A64["mov_x1_cpacr"])
            exception(i, "A64_CPACR_WRITE", words(msr("cpacr_el1", 1)))
            i.word(A64["ret"])
        img.func(f"enable_fp_{n}", cpacr)
    emit_fn(o.get("user"))(img)
    img.func("after_user", emit_fn(o.get("after_user", words(A64["ret"]))))
    if "records" in o:
        img.records = o["records"](img.records)
    return img


def a64_smp2(record="USER_PROGRAM smp2_witness"):
    def body(i, k):
        i.sym("$x")
        i.word(A64["fmov_d0_x1"])
        i.word(A64["svc"])
        i.sym("$d")
        i.emit(b"NR6-REQ!")
        i.sym("$x")
    segs = [("yarm_smp2_s_start", "yarm_smp2_s_end"), ("yarm_smp2_c_start", "yarm_smp2_c_end"),
            ("yarm_smp2_h_start", "yarm_smp2_h_end")]
    return lambda img: program(img, segs, body, record, fill=b"\0")


# ── RISC-V ─────────────────────────────────────────────────────────────────────────────────────

RV_BOOT = 0x80200000
RV_TEXT = 0x80201000
RV = dict(ret=0x00008067, nop=0x00000013, add=0x00C58533, addi_dn=0xFF010113, addi_up=0x01010113,
          fld=0x00053007, ecall=0x00000073)


def rv_image(o):
    img = Image("riscv64")
    img.section(".text.boot", RV_BOOT, SHF_ALLOC | SHF_EXEC)
    img.func("_start", words(RV["nop"], RV["ret"]))
    img.section(".text", RV_TEXT, SHF_ALLOC | SHF_EXEC)

    def kernel_main(i):
        i.word(RV["addi_dn"])
        emit_fn(o.get("ordinary"))(i)
        i.word(RV["add"])
        i.word(RV["addi_up"])
        i.word(RV["ret"])
    img.func("kernel_main", kernel_main)
    emit_fn(o.get("user"))(img)
    img.func("after_user", emit_fn(o.get("after_user", words(RV["ret"]))))
    if "records" in o:
        img.records = o["records"](img.records)
    return img


def rv_prog(segs, record):
    def body(i, k):
        i.sym("$x")
        i.word(RV["fld"])
        i.word(RV["ecall"])
    return lambda img: program(img, segs, body, record, fill=b"\0")


RV_SMP3 = [("yarm_smp3_s_start", "yarm_smp3_s_end"), ("yarm_smp3_c_start", "yarm_smp3_c_end"),
           ("yarm_smp3_h_start", "yarm_smp3_h_end")]
RV_VMI = [("yarm_vmiw_r_start", "yarm_vmiw_r_end"), ("yarm_vmiw_o_start", "yarm_vmiw_o_end")]


# ── cases ──────────────────────────────────────────────────────────────────────────────────────
#
# name -> (arch the checker is run for, features, image builder, disassembler stub mode or "").

CASES = {}


def case(name, arch, features="", stub=""):
    def reg(fn):
        CASES[name] = (arch, features, fn, stub)
        return fn
    return reg


def x86_case(name, features="", stub="", **o):
    case(name, "x86_64", features, stub)(lambda: x86_image(o))


def recorded(hexbytes, name):
    """Ordinary-code bytes covered by a map record `name`."""
    def f(i):
        a = i.here()
        i.emit(hexbytes)
        i.record(a, i.here(), name)
    return f


def edit(name, fn):
    """Map-record post-processing: apply fn to every record named `name`."""
    return lambda recs: [fn(r) if r[2] == name else r for r in recs]


def shift(name, ds, de):
    return edit(name, lambda r: (r[0] + ds, r[1] + de, r[2]))


# The two reproduced false passes of the delivered checker, and their control.
x86_case("x86_baseline")
x86_case("x86_ordinary_fninit", ordinary="db e3")
x86_case("x86_rexwb_fninit", ordinary="49 db e3")                   # rex.WB fninit
x86_case("x86_data_arbitrary", ordinary=recorded("db e3", "DATA arbitrary"))

# Prefixed FP and FP-state operations in ordinary code: every one must be found.
for _n, _b in [("rex_fninit", "40 db e3"), ("rexr_fninit", "44 db e3"),
               ("rexx_fninit", "42 db e3"), ("rexrb_fninit", "45 db e3"),
               ("rexwrxb_fninit", "4f db e3"), ("rex2_fninit", "d5 00 db e3"),
               ("rex2w_fninit", "d5 08 db e3"), ("data16_rex_fninit", "66 40 db e3"),
               ("fs_rexwb_fninit", "64 49 db e3"), ("repz_rexwb_fninit", "f3 49 db e3"),
               ("lock_fninit", "f0 db e3"), ("bare_rexw_then_rexb_fninit", "48 41 db e3"),
               ("rexw_ldmxcsr", "48 0f ae 10"), ("rexwr_ldmxcsr", "4c 0f ae 10"),
               ("rexb_emms", "41 0f 77"), ("rexb_vzeroupper", "41 c5 f8 77"),
               ("rex_stmxcsr", "40 0f ae 18"), ("rexwb_fnsave", "49 dd 30"),
               ("rexwb_frstor", "49 dd 20"), ("rexwb_finit", "9b 49 db e3"),
               ("rexwb_cr0_write", "49 0f 22 c0"), ("rex_clts", "40 0f 06"),
               ("rexw_xsetbv", "48 0f 01 d1")]:
    x86_case(f"x86_{_n}", ordinary=_b)

# Ordinary integer instructions with legitimate prefixes: must pass.
x86_case("x86_prefixed_integer_ok", ordinary=" ".join([
    "f0 48 01 01",                  # lock add %rax,(%rcx)
    "f3 48 ab",                     # rep stos %rax,%es:(%rdi)
    "66 66 2e 0f 1f 84 00 00 00 00 00",  # data16 cs nopw 0x0(%rax,%rax,1)
    "f2 f0 48 01 01",               # xacquire lock add
    "f3 f0 48 01 01",               # xrelease lock add
    "40 90",                        # rex nop
    "49 89 c0",                     # mov %rax,%r8 (REX absorbed)
    "d5 18 89 c0",                  # mov %rax,%r16 (REX2)
    "d5 00 90",                     # {rex2 0x0} nop
    "3e 74 00",                     # je,pt
    "f2 74 00",                     # bnd je
    "67 e8 00 00 00 00",            # addr32 call
    "48 f0 48 01 01",               # rex.W (alone) ; lock add
    "64 48 8b 04 25 28 00 00 00",   # mov %fs:0x28,%rax
    "2e 3e 90",                     # cs ds nop
    "66 f3 90",                     # data16 pause
    "3e ff d0",                     # notrack call *%rax
    "f2 ff d0",                     # bnd call *%rax
    "66 48 0f 1f 44 00 00",         # data16 nopq
]))

# Decoder output the checker cannot interpret (a stub objdump rewrites the real output).
for _m in ["passthrough", "unknown-prefix", "extra-tokens", "bad-pseudo-prefix", "garbage-line",
           "dropped-line", "fails"]:
    x86_case(f"x86_stub_{_m.replace('-', '_')}", stub=_m)
x86_case("x86_trailing_partial_insn", text_end="0f")
# The disassembler itself: missing, an unsupported version, or decoding the checker's probe
# differently from the grammar it was written against (BUILD-GATES1).
x86_case("x86_tools_missing_objdump", stub="missing-objdump")
x86_case("x86_tools_unsupported_version", stub="old-version")
x86_case("x86_tools_probe_misdecoded", stub="garble-probe")
case("x86_truncated_elf", "x86_64")(lambda: Truncated(x86_image({})))

# Exclusions: boundaries, presence, ownership, overlaps, boot mode.
x86_case("x86_excl_handoff_enlarged", after_trampoline="db e3 c3",
         records=shift("DATA ap_trampoline_handoff", 0, 2))
x86_case("x86_excl_gdt_shifted", records=shift("DATA ap_trampoline_gdt", 2, 2))
x86_case("x86_excl_duplicate_boot32",
         records=lambda rs: rs + [r for r in rs if r[2] == "CODE32 boot32"])
x86_case("x86_excl_missing_boot32", records=lambda rs: [r for r in rs if r[2] != "CODE32 boot32"])
x86_case("x86_excl_eliminated_boot32", records=edit("CODE32 boot32", lambda r: (0, 0, r[2])))
x86_case("x86_excl_conflict_exception_in_data",
         handoff=bytes(range(1, 9)) * 16 + bytes.fromhex("0f22c0") + b"\0" * 5,
         records=lambda rs: _move_first_tramp_cr0(rs))
x86_case("x86_excl_boot32_not_identity", boot32_addr=0xFFFFFFFF80100020)
x86_case("x86_excl_trampoline_oversize", code32_pad="90" * 4096)
x86_case("x86_excl_unknown_user_program", ordinary=recorded("db e3", "USER_PROGRAM bogus_witness"))
x86_case("x86_user_vmi2_ok", "x86-vmi2-witness", user=x86_vmi2())
x86_case("x86_user_overtaken_ok", "x86-overtaken-witness", user=x86_overtaken())
x86_case("x86_user_enlarged", "x86-vmi2-witness", user=x86_vmi2(), after_user="db e3 c3",
         records=shift("USER_PROGRAM vmi2_witness", 0, 2))
x86_case("x86_user_mislabeled_kernel_code", "x86-vmi2-witness", user=x86_vmi2(None),
         ordinary=recorded("db e3", "USER_PROGRAM vmi2_witness"))
x86_case("x86_user_anchors_around_kernel_function", "x86-vmi2-witness",
         user=lambda img: _anchored_kernel_function(img))
x86_case("x86_user_without_feature", "", user=x86_vmi2())
x86_case("x86_user_record_missing", "x86-vmi2-witness", user=x86_vmi2(None))
x86_case("x86_user_eliminated_ok", "x86-vmi2-witness",
         records=lambda rs: rs + [(0, 0, "USER_PROGRAM vmi2_witness")])
x86_case("x86_user_eliminated_but_linked", "x86-vmi2-witness", user=x86_vmi2(),
         records=edit("USER_PROGRAM vmi2_witness", lambda r: (0, 0, r[2])))
x86_case("x86_user_segments_not_contiguous", "x86-overtaken-witness",
         user=x86_overtaken(gap="db e3"))
x86_case("x86_user_anchor_defined_twice", "x86-vmi2-witness", user=x86_vmi2(),
         symbols=lambda ss: ss + [s for s in ss if s[0] == "yarm_vmi2_start"])

# Malformed metadata.
x86_case("x86_meta_bad_map_word", map_tail=struct.pack("<Q", 0xDEADBEEF))
x86_case("x86_meta_truncated_map_record", map_tail=struct.pack("<QQ", 0x3150464F4E4D5259, 0))
x86_case("x86_meta_malformed_record_name", ordinary=recorded("90", "data Arbitrary!"))
x86_case("x86_meta_inverted_record", records=edit("X86_SWITCH_SAVE", lambda r: (r[1], r[0], r[2])))
x86_case("x86_meta_unknown_feature", "no-such-feature")
case("x86_meta_wrong_machine", "aarch64")(lambda: x86_image({}))


def _move_first_tramp_cr0(recs):
    """Re-point the trampoline's 16-bit CR0 exception at the copy hidden in the handoff DATA."""
    out, moved = [], False
    tramp = next(r for r in recs if r[2] == "DATA ap_trampoline_handoff")
    for r in recs:
        if not moved and r[2] == "X86_CR0_WRITE" and r[0] >= X86_TEXT and r[0] < tramp[0] - 0x40:
            if any(c[2] == "CODE16 ap_trampoline" and c[0] <= r[0] < c[1] for c in recs):
                r = (tramp[0] + 128, tramp[0] + 131, r[2])
                moved = True
        out.append(r)
    return out


def _anchored_kernel_function(img):
    """The vmi2 anchors placed around a sized KERNEL function that uses x87."""
    img.align(16, b"\xcc")
    s = img.here()
    img.sym("yarm_vmi2_start", bind="global")
    img.func("kernel_helper", emit_fn("db e3 c3"))
    img.sym("yarm_vmi2_end", bind="global")
    img.record(s, img.here(), "USER_PROGRAM vmi2_witness")


class Truncated:
    def __init__(self, img):
        self.img = img

    def write(self, path):
        self.img.write(path)
        with open(path, "rb") as f:
            data = f.read()
        with open(path, "wb") as f:
            f.write(data[:len(data) // 3])


# AArch64.
def a64_case(name, features="", **o):
    case(name, "aarch64", features)(lambda: a64_image(o))


a64_case("a64_baseline")
case("a64_tools_missing_llvm_objdump", "aarch64", stub="missing-llvm-objdump")(lambda: a64_image({}))
a64_case("a64_ordinary_fmov", ordinary=words(A64["fmov_d0_x1"]))


def _a64_data_word(i):
    """An FP instruction hand-encoded as data inside kernel code: a `$d` span nothing loads."""
    i.sym("$d")
    i.word(A64["fmov_d0_x1"])
    i.sym("$x")


a64_case("a64_data_not_a_literal", ordinary=_a64_data_word)
a64_case("a64_data_pool_partly_loaded", pool_extra=4)
a64_case("a64_user_smp2_ok", "aarch64-smp2-witness", user=a64_smp2())
a64_case("a64_user_lock2_ok", "aarch64-lock2-witness", user=a64_smp2())
a64_case("a64_user_enlarged", "aarch64-smp2-witness", user=a64_smp2(),
         after_user=words(A64["fmov_d0_x1"], A64["ret"]),
         records=shift("USER_PROGRAM smp2_witness", 0, 4))
a64_case("a64_user_without_feature", "", user=a64_smp2())
a64_case("a64_excl_data_record", ordinary=lambda i: _a64_recorded(i, "DATA literal_pool"))
a64_case("a64_excl_x86_region_kind", ordinary=lambda i: _a64_recorded(i, "CODE16 ap_trampoline"))


def _a64_recorded(i, name):
    a = i.here()
    i.word(A64["fmov_d0_x1"])
    i.record(a, i.here(), name)


# RISC-V.
def rv_case(name, features="", **o):
    case(name, "riscv64", features)(lambda: rv_image(o))


rv_case("rv_baseline")
rv_case("rv_ordinary_fld", ordinary=words(RV["fld"]))
rv_case("rv_user_vmi_ok", "riscv64-vmi-witness",
        user=rv_prog(RV_VMI, "USER_PROGRAM vmi_witness"),
        records=lambda rs: rs + [(0, 0, "USER_PROGRAM smp3_witness")])
rv_case("rv_user_smp3_ok", "riscv64-smp3-witness", user=rv_prog(RV_SMP3, "USER_PROGRAM smp3_witness"))
rv_case("rv_user_eliminated_without_feature",
        records=lambda rs: rs + [(0, 0, "USER_PROGRAM smp3_witness")])
rv_case("rv_user_mislabeled_kernel_code", "riscv64-smp3-witness",
        user=rv_prog(RV_SMP3, None),
        ordinary=lambda i: (i.record(i.here(), i.here() + 4, "USER_PROGRAM smp3_witness"),
                            i.word(RV["fld"])))


def _rv_data(i):
    i.sym("$d")
    i.word(RV["fld"])
    i.sym("$x")


# LLVM's RISC-V disassembler decodes `$d` spans as instructions, so a mapping symbol cannot hide
# an FP instruction there: it is still found.
rv_case("rv_mapping_data_still_decoded", ordinary=_rv_data)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("cases", nargs="*")
    a = ap.parse_args()
    if a.list:
        for n in CASES:
            print(n)
        return 0
    if not a.out:
        ap.error("--out is required")
    os.makedirs(a.out, exist_ok=True)
    names = a.cases or list(CASES)
    with open(os.path.join(a.out, "cases.tsv"), "w") as m:
        for n in names:
            arch, feats, fn, stub = CASES[n]
            fn().write(os.path.join(a.out, f"{n}.elf"))
            m.write(f"{n}\t{arch}\t{feats or '-'}\t{stub or '-'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
