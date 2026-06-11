# Linux is Obsolete.

There. Someone had to say it again.

For those of you who weren't around in January 1992, a mild-mannered professor named Andrew Tanenbaum posted a message to comp.os.minix with that exact subject line, arguing that monolithic kernels were a **"catastrophic design mistake"** and that the future belonged to microkernels. A Finnish student named Linus Torvalds disagreed. Loudly. The ensuing flame war was so legendary that operating systems researchers still cite it in academic papers, which is the nerd equivalent of getting a star on the Hollywood Walk of Fame.

Tanenbaum was right about the architecture. He was just about 30 years early on the timeline, which in software terms means he was *practically clairvoyant*.

The monolith won the market. We accepted the bargain: raw performance in exchange for a kernel where a buggy Wi-Fi driver can take down your entire system, where a memory corruption in a filesystem module cascades into a security vulnerability, where the "principle of least privilege" is more of a polite suggestion than an enforced guarantee.

But the original argument never went away. It just waited.

---

<div align="center">

# YARM

**Yet Another Real-time Microkernel** &nbsp;·&nbsp; *or* &nbsp;·&nbsp; **Yet Another Rust Microkernel**

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Language](https://img.shields.io/badge/language-Rust%20(no__std)-orange.svg)](https://www.rust-lang.org/)
[![Architectures](https://img.shields.io/badge/arch-x86__64%20%7C%20AArch64%20%7C%20RISC--V%2064-green.svg)](#)
[![Tests](https://img.shields.io/badge/tests-828%20passing-brightgreen.svg)](#)
[![Status](https://img.shields.io/badge/status-active%20development-yellow.svg)](#)

*A capability-based, formally-auditable microkernel for the systems that can't afford to fail.*

</div>

---

## What is YARM?

YARM is a microkernel written from scratch in **safe, no_std Rust**, designed for the domains where software defects aren't bugs — they're incidents. Safety-critical aerospace avionics. Automotive ECUs under ISO 26262. Industrial real-time control systems. The kind of software that sits between a correct decision and a very bad one.

It is not a toy. It is not a research prototype with a flashy paper and an abandoned GitHub repo. It is a principled, production-aimed kernel built around three non-negotiable convictions:

1. **The kernel must be small.** Not "reasonable." *Small.* Every line of code in the TCB is a liability. YARM's trusted computing base is a microkernel in the original sense — IPC, capability enforcement, scheduling, and nothing else. Drivers live in userspace. Filesystems live in userspace. The kernel does not trust them and neither should you.

2. **Isolation must be enforced by hardware, not convention.** YARM uses a **capability-based security model** with L4-family IPC semantics. Processes do not share objects by default. They do not inherit authority by default. Every access to every resource is mediated by an unforgeable, revocable capability token — no ambient authority, no confused deputy, no "just check the UID."

3. **Rust is not a style choice.** It is a safety argument. The absence of `unsafe` in the kernel core is not an aesthetic preference — it is a property that can be audited, verified, and cited in a safety case. YARM is being built with **DO-178C and ISO 26262 (ASIL-D) certification pathways** in mind from day one. The kernel's `unsafe` block count has been systematically reduced from over 200 to **36**, and the work continues.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Userspace                            │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ │
│  │  Supervisor  │  │   Process    │  │       init        │ │
│  │   (TID 2)    │  │  Mgr (TID 3) │  │     (TID 1)       │ │
│  └──────┬───────┘  └──────┬───────┘  └────────┬──────────┘ │
│         │                 │                    │            │
│         └─────────────────┴────────────────────┘           │
│                    IPC v2 / Capabilities                    │
├─────────────────────────────────────────────────────────────┤
│                      YARM Microkernel                       │
│  ┌───────────┐  ┌──────────────┐  ┌───────────────────┐    │
│  │Capability │  │  IPC v2      │  │    Scheduler      │    │
│  │  Manager  │  │(64B inline,  │  │ (preemptive, SMP) │    │
│  │           │  │  reply caps) │  │                   │    │
│  └───────────┘  └──────────────┘  └───────────────────┘    │
│  ┌───────────┐  ┌──────────────┐  ┌───────────────────┐    │
│  │  Frame    │  │  Demand      │  │  ELF Loader /     │    │
│  │Allocator  │  │  Paging +    │  │  CPIO initramfs   │    │
│  │           │  │  CoW Fork    │  │                   │    │
│  └───────────┘  └──────────────┘  └───────────────────┘    │
│  ┌───────────┐  ┌──────────────┐  ┌───────────────────┐    │
│  │  Guard    │  │  ELF W^X     │  │  SMP / AP         │    │
│  │  Pages    │  │  Enforcement │  │  Trampoline       │    │
│  └───────────┘  └──────────────┘  └───────────────────┘    │
├─────────────────────────────────────────────────────────────┤
│          Hardware:  x86_64  │  AArch64  │  RISC-V 64       │
└─────────────────────────────────────────────────────────────┘
```

### Core Design Properties

| Property | Description |
|---|---|
| **Security model** | Capability-based, L4 IPC semantics |
| **Kernel language** | Rust (`no_std`), `unsafe` blocks: **36** (down from 217+) |
| **IPC** | IPC v2 — synchronous rendezvous, 64-byte inline payload, reply capabilities |
| **Memory management** | Demand paging, Copy-on-Write fork, guard pages, ELF W^X enforcement |
| **Scheduling** | Preemptive, SMP-aware; per-CPU structures in active development |
| **Boot topology** | Three-task userspace: supervisor (TID 2), process manager (TID 3), init (TID 1) |
| **Initramfs** | CPIO-based; ELF loader integrated |
| **POSIX path** | musl libc target; ~13 syscalls to `hello world`, ~43 to BusyBox `sh` |
| **Primary target** | x86_64 (QEMU + bare metal, SMP) |
| **Secondary targets** | AArch64 (Cortex-A72, QEMU virt, RPi 4/5), RISC-V 64 (VisionFive 2) |
| **Test baseline** | 828 passing tests |
| **Binary size** | ~300 KB current → 110–130 KB target (fat LTO, `panic = "abort"`) |
| **License** | Apache 2.0 |

---

## Why Not [Insert Existing Kernel Here]?

**Why not Linux?** Because you just read the intro. But more concretely: Linux is a 30-million-line monolith with a TCB roughly the size of a city. It was not designed for formal verification, and retrofitting safety arguments onto it is a perpetual exercise in futility. It is brilliant engineering for the problem it solves. That problem is not "certifiable safety-critical embedded systems."

**Why not seL4?** seL4 is the gold standard and a towering achievement — a formally verified microkernel with machine-checked proofs. If you need seL4, use seL4. YARM does not yet compete with a decades-long formal verification effort. What it offers is a **modern Rust-native design** that makes the verification path shorter, and a codebase that embedded and systems engineers can read, audit, and contribute to without a PhD in Isabelle/HOL.

**Why not Zephyr / FreeRTOS / RTEMS?** Those are RTOSes, not microkernels. The security boundary guarantees are fundamentally different. YARM enforces hardware isolation between all components, including the OS services themselves. A bug in a Zephyr driver can corrupt kernel state. In YARM, a bug in a userspace driver server crashes that server, and nothing else.

**Why Rust?** Because memory safety bugs account for [~70% of CVEs](https://www.chromium.org/Home/chromium-security/memory-safety/) in systems software. Because `rustc` catches entire classes of concurrency and lifetime bugs at compile time that C lets through to production. Because a safety case for a system written in safe Rust is shorter, cheaper to audit, and more defensible to a certification authority than one written in C. And because systematically driving `unsafe` down to a countable, reviewable set of blocks is something you simply cannot do in C — the entire language is `unsafe`.

---

## Project Status

YARM is in **active development** and has moved well beyond a boot stub. The kernel runs a real three-task userspace topology under QEMU on both x86_64 and AArch64, with a 828-test suite passing. Current work focuses on global lock decomposition (per-CPU scheduler structures) and completing the `execve`-equivalent flow to load ELF binaries from the CPIO initramfs.

### Kernel Core
- [x] Multiarch boot (x86_64, AArch64)
- [x] Higher-half virtual memory, 4-level page tables
- [x] Frame allocator, physical memory management
- [x] Capability system & per-process capability spaces
- [x] IPC v2 — 64-byte inline payload, reply capabilities
- [x] Preemptive scheduler, SMP-aware
- [x] SMP AP trampoline (x86_64) — APs reach 64-bit mode
- [x] LAPIC, interrupt controller (x86_64)
- [x] Per-CPU structures (in active decomposition)

### Memory & Process Model
- [x] Demand paging
- [x] Copy-on-Write fork
- [x] Guard pages
- [x] ELF W^X enforcement
- [x] CPIO initramfs loader
- [x] ELF loader (userspace binary loading)
- [x] Three-task boot topology (supervisor / process manager / init)
- [ ] `handle_spawn_process` — load ELF from initramfs end-to-end (in progress)

### Safety & Code Quality
- [x] `unsafe` block count: **36** (down from 217+)
- [x] 828-test baseline, maintained across refactors
- [ ] Per-subsystem lock decomposition (Stage 28 of decomposition plan, in progress)
- [ ] Formal TCB boundary audit
- [ ] `#![forbid(unsafe_code)]` on non-HAL kernel crates (long-term goal)

### Platform Support
- [x] x86_64 — QEMU, SMP, ring3 userspace executing
- [x] AArch64 — QEMU `virt`, Cortex-A72
- [ ] AArch64 — Raspberry Pi 4 (BCM2711) bare metal
- [ ] AArch64 — Raspberry Pi 5 (BCM2712 / RP1) bare metal
- [ ] RISC-V 64 — VisionFive 2 (StarFive JH7110S)

### POSIX Compatibility Roadmap
- [ ] ~13 syscalls for musl `hello world`
- [ ] ~30 additional syscalls for BusyBox `sh`
- [ ] `execve`, `clone`, signal delivery (identified as the three hardest)
- [ ] POSIX signal architecture: hybrid kernel-assisted / userspace dispatch thread (designed, not yet implemented)

`YARM_BOOT_OK` has been printing across two architectures for a while now. We're past the hard part of existence — now we work on the hard part of correctness.

---

## Building

### Prerequisites

- Rust nightly toolchain
- `cargo` with cross-compilation targets
- `qemu-system-x86_64` / `qemu-system-aarch64`
- `llvm-tools-preview` component

```bash
rustup target add x86_64-unknown-none
rustup target add aarch64-unknown-none
rustup component add llvm-tools-preview
```

### x86_64

```bash
cargo build --target x86_64-yarm-none.json --release
qemu-system-x86_64 -kernel target/x86_64-yarm-none/release/yarm -serial stdio -m 512M
```

### AArch64

```bash
cargo build --target aarch64-yarm-none.json --release
qemu-system-aarch64 -M virt -cpu cortex-a72 \
  -kernel target/aarch64-yarm-none/release/yarm \
  -serial stdio -m 2048M
```

---

## Performance Targets

YARM is not optimized for benchmarks. It is optimized for auditability, isolation, and correctness. That said, performance matters for real-time guarantees — a microkernel with slow IPC is a microkernel you cannot use.

Current IPC baseline on Cortex-A72: **~450–600 cycles**. Target after fast-path optimization: **300–350 cycles**. The optimization roadmap includes:

- ASID 0 pinning on AArch64 to eliminate unnecessary TLB flushes
- IPC fast/slow path split with `#[cold]`-annotated slow path
- Canonical `&'static SharedKernel` via `AtomicPtr` (eliminating repeated lock acquisition)
- Per-CPU scheduler structures (in progress as part of the global-lock-removal plan)

Binary size target: **110–130 KB** (from ~300 KB current), using fat LTO, `codegen-units = 1`, `panic = "abort"`, and a planned `yarm-kernel-cold` crate split for rarely-executed paths.

---

## Design Philosophy

YARM is not being built to replace Linux in the general-purpose computing market. That war is over, and the monolith won, and it was probably the right call for that domain.

YARM is being built for the systems that live at the intersection of **high consequence** and **high assurance** — where the cost of a failure is measured not in user frustration, but in something more significant. The kernel at the heart of a flight management computer. The hypervisor isolating safety partitions in an automotive ECU. The secure enclave managing cryptographic material for critical infrastructure.

For those systems, the Tanenbaum argument was never really answered. It was just deferred.

YARM is the resumption of that argument, in Rust, with modern tooling and three decades of hindsight.

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Commercial licensing and support arrangements — including certification artifact packages for DO-178C and ISO 26262 (ASIL-D) qualification — are planned. If you are evaluating YARM for a safety-critical program, open an issue or reach out directly.

---

## Contributing

YARM is a solo project in active development. Contributions, issues, and design-level feedback are welcome. Please read `CONTRIBUTING.md` before submitting patches. A Contributor License Agreement will be required — this is intentional and in service of the long-term commercialization and certification strategy.

---

<div align="center">

*"MINIX is more portable, better designed, and runs on cheaper hardware… but I guess that's not the point, is it?"*
— Andrew S. Tanenbaum, 1992

He was right. He was just writing it in C.

</div>
