<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM development history (2026-02-25 → 2026-11-04)

Chronological, evidence-labelled. Labels: **[VC]** verified in source at `b81e0317`; **[VH]**
verified from commits/committed records; **[SR]** session-reported; **[INF]** inferred;
**[UNK]** unknown. Commit SHAs are abbreviated from the full history (`main` has 3 186 commits).
The kernel-unlocking line is told in depth in `doc/YARM_KERNEL_UNLOCK_HISTORY.md`; bugs in
`doc/YARM_BUGS_AND_LESSONS.md`; sources in `doc/YARM_RECOVERY_SOURCES.md`.

## Eras at a glance [VH]

| Era | Dates | Who committed | Delivery style |
|---|---|---|---|
| 1. Foundations | 02-25 → 04-30 | owner account (Codex-driven PRs from #1) | PRs (`codex/*`) and direct commits |
| 2. Userspace bring-up, IPC phases | 05-01 → 06-10 | owner + Claude (from 05-11) | PRs (`claude/*`, some `codex/*`) |
| 3. Kernel Unlocking Milestones, D6 bring-up | 06-11 → 06-30 | Claude | PRs, numbered Stages 101–167 |
| 4. Graduation, cohorts, direct IPC, timeouts | 07-01 → 08-10 | Claude | PRs, Stages 168–200D, 199D (last PR #542, 08-10) |
| 5. U-series and U9 closure | 08-11 → 09-24 | Claude (one long session) | direct fast-forward to `main` and `claude/yarm-kernel-unlock-u0-f2zh8p`, WIP refs kept |
| 6. QEMU SMP evidence | 09-25 → 10-08 | Claude | same |
| 7. Backlog, deletion, production SMP | 10-09 → 10-21 | Claude | same, owner briefs per package |
| 8. Scheduling contract, lock split, kernel threads | 10-21 → 11-04 | Claude | same |

---

## Era 1 — Foundations (Feb–Apr 2026)

* **2026-02-25 `3088f528`** "Initialize repository"; PR #1 (Codex,
  `codex/implement-vm-primitives-in-microkernel`): initial microkernel core and arch split —
  RISC-V trap decoder, IPC, capabilities, VM, syscalls, Linux-compat. `c1da5b88` same day:
  Linux-compat VM semantics, compat syscall dispatch, `brk`. [VH]
* **March:** RISC-V trap adapter and SMP-safe shared runtime (`70d49b7a`); Linux process-manager
  IPC for getpid/exit; syscall IPC ABI hardening (`0ee85bf7`); bootstrap task policy; process
  manager lifecycle (`a7bf72fc`); **first bootable raw x86 kernel ELF (`0749ca12`, 03-23)**;
  cspace guardrail scanning; control-plane VFS loop over kernel IPC; firmware-blob boot
  configuration; runtime capacity limits across subsystems (`5dee17d4`); cache-policy plumbing;
  x86 `.bss` zeroing. PRs #2–#5 ("IPC async fastpath with direct mailbox delivery") were closed
  unmerged. [VH]
* **April:** thread-derived cnode constructor removed (Phase B); int 0x80 syscall gate; runtime
  init handed to the control-plane init service (`1b23a54d`); transfer-release syscall;
  **SchedulerState extraction complete (`ac180f38`, 04-01)**; Apache-2.0 LICENSE and licensing
  rule (`7565c8d9`); frame allocator under `SpinLockIrq`; supervisor budget-aware receive and
  reply-cap status path (Phase 6); legacy PIC masked on PVH; initramfs core-service manifest;
  AP trampoline fixes; **copy-on-write fork address-space cloning (`a20b426a`, 04-09)**; AArch64
  DTB parsing, custom target/linker, early `.bss` zeroing (04-10/11); platform-neutral
  bootstrap state; RISC-V custom target and linker (`85430e55`, 04-20); AArch64 SMP identity
  from MPIDR; driver services moved to `yarm-driver-servers`; POSIX hooks over IPC boundary;
  supervisor restart/redelegation isolated from kernel state; **first locking-status document
  (`b21bb013`, 04-30)**. [VH]

Architectural decisions visible in this era [INF from commit subjects and `doc/STATUS.md` §3]:
mechanism in the kernel, policy in userspace services (extracted domain crates); capability
authority only through the IPC transferred-cap path; three architectures from the start.

## Era 2 — Userspace bring-up and the IPC phases (May – early June)

* 05-01 → 05-03: staged shared trap seam on x86 (`78129f5d`), startup slot 3 = supervisor fault
  endpoint, AArch64 first-user diagnostics, x86 AP ready-handoff fixes. [VH]
* **05-11 first Claude commit** (`f224d184`, user-rt PM service receive endpoint + `ipc_reply`).
  May 12–21: supervisor startup ABI fixes; the `claude/fix-userspace-startup-bug-lEhmB` PR train
  (#87 → #137). [VH]
* 05-22 → 05-24: syscall ABI decode regression tests; frozen blkcache IPC ABI; `ipc_recv_v2`
  framing/meta contract finalized ("trust out-meta write over stale blocked-path ret0"), local
  reply cap materialized for blocked recv-v2. [VH]
* Recorded phases (from `doc/PROJECT_HISTORY.md`, last edited `28debeab`): Phase 0 IPC baseline
  gates (five conformance tests); Phase 1 `Message::MAX_PAYLOAD = 128` frozen, fragmentation for
  medium payloads, shared-memory descriptor for large; Phase 2A direct NR 27 bridge (superseded);
  **Phase 2B VFS-mediated bulk read frozen 2026-05-27**; Phase 3A InitramfsFileSlice
  MemoryObject cap grant; **Phase 3B page-aligned zero-copy ELF loading proven 2026-05-27**
  (`zc_pages > 0` for image ids 7/8/9 on AArch64 and x86 `-smp 1`); Phase 4 call/reply
  capability (`CapObject::Reply`, single-use, generation-protected); Phase 6 control-plane
  service migration with soft/hard sunsets 2026-06-30 / 2026-09-30; P2.8–P2.10 page-table and
  frame-allocator scaling and strict ISA smoke jobs; TID allocation policy (4 phases). [VH]
* "Milestone 1: PM lifecycle table and full spawner_cap capture" (`777210c8` 05-21 …
  `85c16174`). Stage 4C strict IPC queued recv split (`9d973591`, 05-30). [VH]
* **Optional FS Milestone 1 (Stage 100)**: all userspace FS servers built/staged/tested; RAMFS
  writable; ext4 read-only live; FAT profile-ready and disabled; filesystem work then **paused**
  (`doc/AI_AGENT_RULES.md` §12.2). [VH]

## Era 3 — Kernel Unlocking Milestones and D6 bring-up (06-11 → 06-30)

**Parallel tracks in the same weeks** (absent from the first draft of this file; added after the
preservation audit):

* **VFS shared-memory I/O, 2026-06-05 → 06-10.** Scaffold and helper-only proofs (`ab157af3`,
  `c74789d2`, `d97809db`, `879429ed`, `bf610a52`, mapper-ABI design `71db75ff`), Stages 63–87:
  plain-receive adoption and readiness audit (63/64 `117f987e`), `VfsWriteSharedBinding` over
  `recv_shared_v3` MAP_READ (65 `caa71919`), gated WRITE_SHARED_REQUEST route (66–68 `a35f12b4`),
  narrow MAP_WRITE enablement (72 `cde6cdd3`), RequesterExit model and
  `VFS_READ_SHARED_REPLY_ENABLED=true` (73/74 `cb0c38b4`), TID-matched exit identity (75), PM-owned
  task-exit notification (76–78), final readiness audit + global enable (78 `e12b55a3`),
  `RecvV3SharedIoMapper` production bridge (79 `86e96a96`), FS server CPIO staging/spawn (80),
  RAMFS byte access (83), RAMFS-only service-loop bridge (84 `1d514805`), RAMFS live route proof
  (85 `6df2710e`), RAMFS live mount (86). Current source [VC]: the flags are `true`, but the
  default `handle_request` still returns `Unsupported` for the shared opcodes; the live route is
  RAMFS-only; FAT shared I/O and live mount are `false`. `doc/VFS.md` (Pass 4, 06-15) describes
  production shared I/O as unsupported and the FS-19 kernel-ABI requirement. [VH/VC]
* **RPi5 high-half bring-up, 2026-06-14 → 06-23** (Codex and Claude PRs): HH-3 Rust boundary and PC
  diagnostics (06-14), low TTBR0 retirement (`27727087`), HH-4 DTB handoff ladder (`bca3d0a2`),
  **HH-5 high-half initrd/allocator/handoff bridge (`1e62310a`, 06-21)** replacing the
  `high_half_initrd_allocator_bridge_not_ready` deferral, FDT `/chosen` walker fix (`1795ced2`),
  **real high-half boot allocator + kernel-entry shim (`bbe6b6c7`)**, allocator init/probe hang
  fixes (`5ee97d86`, `4a83117b`, `aca6fe42`), **BOOT-4 high-half kernel heap + VM validation
  (`7eb743de`)**, gated phys↔virt direct map wiring the global allocator (`ec1a063d`, 06-22), the
  trampoline `.bss`-zeroing root-cause fix (`d1374f9a`, 06-22), mock-safe RPi5 driver scaffolding
  (`8f8d98a6`, 06-23). No RPi5 commit follows; the last one reports an artifact build emitting
  all markers, not a hardware run, so **how far hardware boots after BOOT-4 is [UNK]**.
  `doc/RPI5_BRINGUP.md` (last edited 06-15) still says HH-5 is deferred on the bridge. [VH]


Stage 101 restart (`3d2cd43f`) → Milestone 1 at Stage 106 (`6241deb7`, 06-12) → Milestone 2
Passes 1–2 (108 `84033c82`, 109 `f8b6418e`) → D-NEXT phase splits 110–116 → x86 D6 unlocked
`switch_frames` proof harness 117–132 → stability and extraction 133–151 → IPC/cap seam audits
153–160 → sender-wake oracle 161–163 (161 honestly DEFERRED) → fork/COW fixes 163D–163P → D6
proof stack fixes 165B–165J (x86 kernel stacks 64 → 128 KiB, `2b5e1ac1`) → D6-SWITCH-A accepted
(166B). Detail: `doc/YARM_KERNEL_UNLOCK_HISTORY.md` §2–§3. Documentation consolidation Passes
1–4 (Pass 4 completed 2026-06-15: seven canonical owner docs). [VH]

## Era 4 — Graduation, cohorts, direct IPC, timeouts (07-01 → 08-10)

* 07-01/02: D6-GENUINE-B and D2-GENUINE recv/send (168–169); audit stages 170–180; **181
  GRADUATE-KNOBS**, **182 REMOVE-FALLBACKS**, **183 SMP-LIVE**, **184 CROSS-ARCH-LIVE**. [VH]
* 07-04 → 07-07: retirement inventory and HARD-STOP audits (185–188); x86 first live AP ring-3
  dispatch and SMP user-dispatch seal (189C6, 189D); AP scheduler loop (190A). [VH]
* 07-07 → 07-13: cohort retirements (191–196) → **first-cohort cross-arch seal (Stage 197,
  `53574ccb`)** → **NR 27 removed, zero-copy mandatory (197A-B `696db6cb`)**. [VH]
* 07-14 → 07-17: fail-closed builds, reply-cap direct-only, shared-region transactions, shared
  region direct 3/3 seal (198*). [VH]
* 07-18 → 07-21: off-lock direct IpcCall/IpcReply (199A*), x86 SMP cross-CPU seals, AP NXE fix;
  7-21 → 7-27: reply/timeout ownership and the **three-arch reply-timeout matrix `72a4ebff`**
  (200A–C); 7-27 → 7-29: server death and `ExitCurrentTask` NR 16 (200D). [VH]
* 07-29 → 08-10: canonical **199D** — ServerDies accounting, x86 ServerDies live cell
  (`f5669cb5`), x86 direct-IPC production default ON (`0b5ec254`), then **WA1-GATE disabled it
  (`029d3562`, 08-05)** until endpoint-waiter ownership was rebuilt (WA2A–WA3C2). Last PR #542
  (`10b91677`, WA3C1, 08-10). Kernel-unlock audit and documentation consolidation Pass 6
  (`f8a3c046`, 07-29): per-stage report files deleted, seals migrated to
  `doc/PROJECT_HISTORY.md`, `tests/doc_fragmentation_guard.rs` added. [VH]

## Era 5 — The U-series and U9 closure (08-11 → 09-24)

One continuous Claude session (`session_01QLpmtT4bLN2gX3DCKAXYp8`) begins: U1/U2 census
subtractions (`26364834`, `7bb73d85`), U3 drain-class retirement (44 → 7), U4, U6, U7 (production
timeout pipeline on three arches, `8275c927`), then U9 — several dozen named increments (U9-A … U9-CLOSURE-ACCEPTANCE) retiring every
remaining terminal-dispatch population (VM entry, transfer, IPC residuals, receive family,
futex wait, timers, page faults, IRQ) until **U9-TERMINAL-FINAL §5 (`0dce4a7f`)** deleted the
last production broad acquisitions and **U9 closed (`93900ae0`)**. **POST-U9-STABILIZATION
`14b61286`** (09-24) settled four carried failures and became the hardware bring-up reference
that every later brief requires preserving (it lives on `refs/heads/wip/post-u9-stabilization`).
[VH]

## Era 6 — QEMU SMP evidence (09-25 → 10-08)

QEMU-BASELINE1 (`83996c50`…`1cfeb889`), QEMU-IRQ1/2/3 (device interrupts on three ports),
QEMU-CONTEXT1, QEMU-SMP1/2/3 with ACCEPTANCE and SEAL rounds, QEMU-LOCK1/2/3. Local checkout's
shallow boundary sits here (`70561d03`, 09-30). [VH]

## Era 7 — Backlog, deletion, production SMP (10-09 → 10-21)

Owner briefs survive verbatim from 10-12 onwards (`doc/YARM_RECOVERY_SOURCES.md` §1).

| Package | Dates | Delivered | Notes |
|---|---|---|---|
| BL1–BL6 backlog | 10-09 → 10-12 | closing ledger `a91632ba` | see bugs ledger |
| SHARED-REGION-OFFSET1 (BL1-a) | 10-12 | `96114d69` | grant offset ABI |
| SHARED-REGION-LIFETIME1 (BL1-b) | 10-12 | `89273030` | per-mapping lifetime references |
| SHARED-REGION-LIFETIME2 (BL1-c) | 10-12/13 | `0d25d699` | invalidation-gated settlement |
| BROAD-LOCK-RETIREMENT1 | 10-13 | `a75b1601` | **broad lock deleted** |
| VM-INVALIDATION1 | 10-13 | `a72c69a7` | typed invalidation completion, quarantine |
| AP-SCHED1 | 10-13/14 | `e9c8173c` | first production AP-scheduling increment (RISC-V, 2 CPUs) |
| AP-SCHED2 | 10-14 → 10-19 | `7a76f789` | all supported RISC-V harts; UART witness repair |
| AP-SCHED3 | 10-19 | `8cb30dd1` | AArch64 production user scheduling (GICv2, ≤ 8) |
| AP-SCHED4 | 10-20/21 | `df5ee7b6` | x86_64 production SMP (APIC map; ≤ 8 per its record [SR]) |
| VM-INVALIDATION2 (+ACC, +SEAL) | 10-21 | `da591ee8`, `9e4d3c49`, `63537414` | x86 dispatch-gap hole; grader repairs |

## Era 8 — Contract, lock split, kernel threads (10-21 → 11-04)

| Package | Delivered | Notes |
|---|---|---|
| SCHED-CONTRACT1 | `e41b3be9` | `doc/SCHEDULING_CONTRACT.md` |
| SCHED-CONTRACT1-CORRECTION | `04b94fc0` | |
| SCHED-OVERFLOW1 | `91030225` | |
| KERNEL-NOFP1 | `fec063af` | no FP/SIMD in kernel code, disassembly-proven |
| KERNEL-NOFP1-ACCEPTANCE | `9b6c889b` | checker false passes closed |
| BUILD-GATES1 | `adf5fa63` | artifact provenance, boot-to-artifact binding |
| SCHED-LOCK1 | `b7f33257` | per-CPU scheduler lock |
| SCHED-LOCK1-ACCEPTANCE | `035d0f56` | |
| SCHED-LOCK1-SEAL | `0e21b2a6` | |
| SMP3-EVIDENCE1 | `298c591b` | |
| KTHREAD1 | `40132e1a` | cooperative kernel threads |
| KTHREAD1-ACCEPTANCE | `a7da04f1` | |
| KTHREAD-CLOSURE1 | A `136e8ae2`, B `a540aef2`, C `fc429b00`, **D `b81e0317`** | five closures; current `main` |

**Checkpoint verification (2026-11-05):** `main`, `claude/yarm-kernel-unlock-u0-f2zh8p` and
`wip/kthread-closure1` all point to `b81e0317`; `wip/kthread-closure1-a/-b/-c` at `136e8ae2`,
`a540aef2`, `fc429b00`; `14b61286` preserved on `wip/post-u9-stabilization`. **No branch has
newer work.** [VH, full clone]

---

## Major design transitions [VH unless marked]

1. **Monolithic broad lock → ranked domain locks → no broad lock.** The `SpinLock<KernelState>`
   served from February; split-mut seams per rank from Stage 108; U3/U9 retired every runtime
   acquisition; BLR1 deleted the mechanism.
2. **Services out of the kernel.** `src/services/*` removed; domain crates
   (`yarm-control-plane-servers`, `-fs-`, `-driver-`, `-network-`, `-ui-`, `-compat-servers`),
   `yarm-server-runtime` narrow boundary, enforced by `scripts/check-crate-graph-boundary.py` and
   `phase5-boundary-gates`.
3. **Copy → zero-copy loading.** Phase 2B transfer buffer → Phase 3B zero-copy ELF → NR 27
   removed (197A-B).
4. **Knob-gated proofs → production defaults.** e.g. direct IPC NR6/NR7: proof-gated (199A) →
   x86 default (`0b5ec254`) → disabled (WA1-GATE) → re-enabled after waiter ownership (WA3C2) →
   all three arches (DIRECT3-CAP-FINAL) [VC: on everywhere today].
5. **BSP-only → production SMP** (AP-SCHED1–4), with the scheduler lock split per CPU
   (SCHED-LOCK1).
6. **Evidence discipline hardened.** From "marker present" smoke checks to artifact-bound boots
   (BUILD-GATES1), graders that reject missing/duplicated/reordered evidence, predeclared
   qualification schedules, and negative controls per repair.
7. **No kernel FP/SIMD** (KERNEL-NOFP1), a prerequisite for cheap kernel context switches and
   kernel threads.
8. **Kernel threads** (KTHREAD1 → KTHREAD-CLOSURE1): cooperative, bounded, guarded stacks;
   not yet used by production code [VC].

## Abandoned or reversed attempts (selected) [VH]

* PRs #2–#5 "IPC async fastpath with direct mailbox delivery" (03-14), closed unmerged.
* `02302bfc` revert of "install trap kernel state before IDT scaffolding" (04-09); `f176bfbe`
  revert of std-backed hosted page-table storage (04-04).
* Phase 2A direct NR 27 bridge → superseded by Phase 2B; NR 27 finally removed at 197A.
* Direct-IPC x86 production default enabled then disabled (WA1-GATE) then re-enabled.
* Many audit stages ended HARD-STOP without code (185B, 185V, 186B, 186D4, 187D): the recorded
  outcome when no safe seam existed.
* Refused seals preserved as records (e.g. `STAGE_199_IPCCALL_REPLY_OFFLOCK_SEAL … result=deferred`)
  in `doc/PROJECT_HISTORY.md`.
