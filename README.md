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
[![Status](https://img.shields.io/badge/status-active%20development-yellow.svg)](#)

*A capability-based, formally-auditable microkernel for the systems that can't afford to fail.*

</div>

---

## What is YARM?

YARM is a microkernel written from scratch in **safe, no_std Rust**, designed for the domains where software defects aren't bugs — they're incidents. Safety-critical aerospace avionics. Automotive ECUs under ISO 26262. Industrial real-time control systems. The kind of software that sits between a correct decision and a very bad one.

It is not a toy. It is not a research prototype with a flashy paper and an abandoned GitHub repo. It is a principled, production-aimed kernel built around three non-negotiable convictions:

1. **The kernel must be small.** Not "reasonable." *Small.* Every line of code in the TCB is a liability. YARM's trusted computing base is a microkernel in the original sense — IPC, capability enforcement, scheduling, and nothing else. Drivers live in userspace. Filesystems live in userspace. The kernel does not trust them and neither should you.

2. **Isolation must be enforced by hardware, not convention.** YARM uses a **capability-based security model** with L4-family IPC semantics. Processes do not share objects by default. They do not inherit authority by default. Every access to every resource is mediated by an unforgeable, revocable capability token — no ambient authority, no confused deputy, no "just check the UID."

3. **Rust is not a style choice.** It is a safety argument. The absence of `unsafe` in the kernel core is not an aesthetic preference — it is a property that can be audited, verified, and cited in a safety case. YARM is being built with **DO-178C and ISO 26262 certification pathways** in mind from day one.

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                     Userspace                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐  │
│  │  Drivers │  │   VFS    │  │ Network  │  │  App   │  │
│  │ (Server) │  │ (Server) │  │ (Server) │  │        │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └───┬────┘  │
│       │              │              │             │       │
│       └──────────────┴──────────────┴─────────────┘      │
│                          IPC / Capabilities               │
├──────────────────────────────────────────────────────────┤
│                     YARM Microkernel                     │
│       ┌──────────┐  ┌──────────┐  ┌────────────────┐    │
│       │Capability│  │  L4-IPC  │  │   Scheduler    │    │
│       │  Manager │  │  Engine  │  │  (Real-time)   │    │
│       └──────────┘  └──────────┘  └────────────────┘    │
│       ┌──────────┐  ┌──────────┐  ┌────────────────┐    │
│       │  Frame   │  │  Page    │  │   SMP / LAPIC  │    │
│       │Allocator │  │  Tables  │  │   (x86_64)     │    │
│       └──────────┘  └──────────┘  └────────────────┘    │
├──────────────────────────────────────────────────────────┤
│         Hardware:  x86_64  │  AArch64  │  RISC-V 64     │
└──────────────────────────────────────────────────────────┘
```

### Core Design Properties

| Property | Description |
|---|---|
| **Security model** | Capability-based, L4 IPC semantics |
| **Kernel language** | Rust (`no_std`, with `#![forbid(unsafe_code)]` goal in TCB) |
| **Scheduling** | Real-time, preemptive, SMP-aware |
| **Memory model** | Hardware-enforced isolation, per-process capability spaces |
| **IPC** | Synchronous rendezvous + async notification channels |
| **POSIX path** | Two-phase: `newlib` (bootstrap) → `musl` (production) |
| **Primary target** | x86_64 (QEMU + bare metal) |
| **Secondary targets** | AArch64, RISC-V 64 |
| **License** | Apache 2.0 |

---

## Why Not [Insert Existing Kernel Here]?

**Why not Linux?** Because you just read the intro. But more concretely: Linux is a 30-million-line monolith with a TCB roughly the size of a city. It was not designed for formal verification, and retrofitting safety arguments onto it is a perpetual exercise in futility. It is brilliant engineering for the problem it solves. That problem is not "certifiable safety-critical embedded systems."

**Why not seL4?** seL4 is the gold standard and a towering achievement — a formally verified microkernel with machine-checked proofs. If you need seL4, use seL4. YARM does not yet compete with a decades-long formal verification effort. What it offers is a **modern Rust-native design** that makes the verification path shorter and a codebase that embedded and systems engineers can read, audit, and contribute to without a PhD in Isabelle/HOL.

**Why not Zephyr / FreeRTOS / RTEMS?** Those are RTOSes, not microkernels. The security boundary guarantees are fundamentally different. YARM enforces hardware isolation between all components, including the OS services themselves. A bug in a Zephyr driver can corrupt kernel state. In YARM, a bug in a userspace driver server crashes that server, and nothing else.

**Why Rust?** Because memory safety bugs account for [~70% of CVEs](https://www.chromium.org/Home/chromium-security/memory-safety/) in systems software. Because `rustc` catches entire classes of concurrency and lifetime bugs at compile time that C lets through to production. Because a safety case for a system written in safe Rust is shorter, cheaper to audit, and more defensible to a certification authority than one written in C.

---

## Project Status

YARM is in **active early development**. The kernel boots on QEMU for both x86_64 and AArch64, with RISC-V 64 support in progress. The following components are implemented and functional:

- [x] Multiarch boot sequence (x86_64, AArch64)
- [x] Frame allocator & physical memory management
- [x] Higher-half virtual memory, page table management
- [x] Capability system & capability space model
- [x] L4-style synchronous IPC primitives
- [x] Preemptive scheduler (SMP-aware)
- [x] Basic syscall dispatch layer
- [x] LAPIC / interrupt controller (x86_64)
- [x] Serial output, early boot diagnostics
- [ ] DTB parsing for AArch64 memory layout
- [ ] RISC-V 64 full boot path
- [ ] Userspace loader & initial server protocol
- [ ] newlib port (POSIX bootstrap layer)
- [ ] Driver server framework
- [ ] Formal TCB boundary audit

This is not vaporware. `YARM_BOOT_OK` has been printed. We're past the hard part of existence — now we work on the hard part of correctness.

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

## Design Philosophy

YARM is not being built to replace Linux in the general-purpose computing market. That war is over, and the monolith won, and it was probably the right call for that domain.

YARM is being built for the systems that live at the intersection of **high consequence** and **high assurance** — where the cost of a failure is measured not in user frustration, but in something more significant. The kernel at the heart of a flight management computer. The hypervisor isolating safety partitions in an automotive ECU. The secure enclave managing cryptographic material for critical infrastructure.

For those systems, the Tanenbaum argument was never really answered. It was just deferred.

YARM is the resumption of that argument, in Rust, in 2025, with modern tooling and three decades of hindsight.

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Commercial licensing and support arrangements available for safety-critical integration — open an issue or reach out directly.

---

## Contributing

YARM is a solo project in early development. Contributions, issues, and design feedback are welcome. Please read `CONTRIBUTING.md` before submitting patches. A Contributor License Agreement will be required — this is intentional and in service of the long-term commercialization and certification strategy.

Before pushing FS service/runtime changes, run:

```bash
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
```

This checks FS runtime-entrypoint parity and FS bins buildability.

Before pushing driver service/runtime changes, run:

```bash
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
```

This checks driver runtime-entrypoint parity and driver bins buildability.

Before pushing network service/runtime changes, run:

```bash
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
```

This checks network runtime-entrypoint parity and network bins buildability.

---

<div align="center">

*"MINIX is more portable, better designed, and runs on cheaper hardware… but I guess that's not the point, is it?"*
— Andrew S. Tanenbaum, 1992

He was right. He was just writing it in C.

</div>
