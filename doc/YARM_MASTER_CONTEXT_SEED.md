<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM master context seed

Principal handoff for any new ChatGPT, Claude or Codex session. Compiled 2026-11-05 from the
full Git history (3 186 commits on `main`), the committed records, the one surviving Claude Code
transcript (Oct 9 – Nov 5 2026) and current source at `b81e0317`; **reviewed and reconciled
2026-11-09** against the owner's original ChatGPT planning conversation (Aug 10 – Nov 4 2026,
1 240 messages; cited as "Msg ####", see `doc/YARM_RECOVERY_SOURCES.md` §2a and
`doc/YARM_STAGE_LEDGER.md`). Evidence labels:
**[VC]** verified in current source; **[VH]** verified from commits/committed records;
**[SR]** session-reported; **[INF]** inferred; **[UNK]** unknown. Details and provenance:
`doc/YARM_DEVELOPMENT_HISTORY.md`, `doc/YARM_KERNEL_UNLOCK_HISTORY.md`,
`doc/YARM_BUGS_AND_LESSONS.md`, `doc/YARM_RECOVERY_SOURCES.md`.

---

## Copy-paste seed

> You are continuing development of the YARM Rust microkernel. The following is the verified
> project state and mandatory engineering context.
>
> **Project.** YARM ("Yet another Rust Microkernel", github.com/umutdenizbalkan/YARM,
> Apache-2.0, owner Umut Deniz Balkan) is a capability-based microkernel for x86_64 (PVH),
> AArch64 (QEMU virt, GICv2) and RISC-V64 (OpenSBI, QEMU virt), developed under QEMU. Strict
> mechanism/policy separation: the kernel owns scheduling, IPC, capabilities, traps/IRQ routing
> and VM mechanisms; all policy lives in userspace service crates (`crates/yarm-*-servers`).
>
> **Current state (main = `b81e0317`, 2026-11-04).** The broad `SpinLock<KernelState>` is
> deleted (BROAD-LOCK-RETIREMENT1); boot owns state via a `BootExclusive` token, then each field
> has one recorded authority under ranked domain locks (1 scheduler — now per-CPU — 2 task,
> 3 IPC, 4 capability, 5 VM, 6 memory, 7 driver, 8 fault, 9 restart, 10 telemetry), acquired in
> ascending order; `tests/broad_lock_census_guard.rs` rejects reintroduction. Production user
> scheduling runs on secondary CPUs on all three ports (production ceiling 8 CPUs per port in
> source; `MAX_CPUS = 64`). Off-lock direct IPC (NR 6/7) is the production default on all three
> ports (since `2aab8b77`, 2026-09-02). Endpoint receive waiters keep **last-receiver-wins**
> semantics with an explicit displacement ownership transition (WA3C2 `612339b5`); strict
> single-waiter publication was tried and rejected (it hangs the VFS grant relay). The kernel is
> non-preemptive; kernel threads exist (cooperative, 32 × 16 KiB, guard-gapped stacks on three
> ISAs) but **no production code creates them yet**. Kernel code uses no FP/SIMD (disassembly
> checker on every build). Hosted suite: 6 275 tests pass serially; 41 integration binaries
> pass. The owner's 35 canonical unlocking stages (199C–205D in `doc/KERNEL_UNLOCKING.md` §0)
> were **not re-classified** after the lock deletion; only 199C, 199E and 204A are recorded
> complete — treat others as open until re-qualified against their definitions. The roadmap
> predates the lock's deletion: many stages' lock premise is now satisfied structurally and their
> transactions were delivered by later U9 directives; what remains is parity, scaffolding removal,
> decomposition and evidence (see `doc/YARM_KERNEL_UNLOCK_HISTORY.md`).
>
> **Next work (recorded roadmap).** RT-SCHED1 (fixed-priority preemption, scheduling contexts and
> budgets, calibrated time, a fair kernel lock or contention bound — scheduling contract SC-LOCK-2
> progress is still PARTIAL), then PARTITION1, FP-POLICY1. VFS is not new work: the VFS server,
> RAMFS shared-memory I/O (RAMFS-only live route) and FAT/ext4 groundwork exist since June; the
> open VFS items are general production shared I/O in the default dispatch, FAT live mount and
> ext4 writes. RPi5 high-half bring-up reached an HH-5 initrd/allocator/kernel-entry bridge and
> a BOOT-4 high-half heap in June (diagnostic, `rpi5-highhalf` feature); whether EL0 was ever
> entered on hardware is unknown. Asynchronous kernel preemption (SC-KT-4) is not enabled.
>
> **How the owner steers work.** The owner's standing priority during unlocking was "kernel
> unlocking is the first priority" — bug fixes only when they block a named step (`BLOCKS: …`),
> every pass naming the metric or dispatcher population it reduces, no open-ended documentation,
> oracle or hardening chains. Unlocking is now done; the owner's later choices were QEMU-first
> validation before hardware, then the real-time/mixed-criticality direction (ISO 26262 /
> DO-178C / ARINC 653 as *motivation*, not a compliance claim), with RPi5 bare-metal bring-up,
> drivers and futex requeue/bitset/PI as stated later goals. Proposals from the owner or an AI
> are reviewed against source and corrected before adoption; record what was proposed, what was
> accepted and what shipped separately.
>
> **Mandatory rules.** Verify every claim against current source before acting (code beats
> comments and docs; several docs are stale). One bounded, independently reviewable issue per
> delivery. Reproduce a defect on the unmodified base first; repair the owning operation; no
> broad/global lock, renamed wrapper, capacity increase, weakened guard, oracle-only fix or ABI
> change as a shortcut. Default-off witnesses may observe production owners, never implement
> the fix. Qualify on x86_64, AArch64 and RISC-V from a frozen commit with fresh artifacts and
> a predeclared schedule; keep every result; a passing retry never replaces a failed run; every
> repair has a negative control that fails without it; hosted tests never prove SMP/hardware
> behaviour. Distinguish hosted foundation / live proof (knob-gated?) / production default /
> canonical stage complete. Preserve `14b61286` and every existing ref; no force push, rebase,
> history rewrite, branch deletion or PR unless the owner asks; deliver by fast-forward after a
> fresh fetch and ancestry check, then verify remote equality. Hosted tests: `CARGO_INCREMENTAL=0`,
> `--test-threads=1`. Never `pkill -f`; kill by PID. Read `doc/AI_AGENT_RULES.md`,
> `doc/KERNEL_UNLOCKING.md` §0/§3/§4 and `doc/SCHEDULING_CONTRACT.md` before changing kernel code.
> Report only numbers your own run produced, measured with the project's tools (census by the
> guard's scanner, integration suites included); a fabricated test count once entered the
> record and had to be retracted. Push green checkpoints to a preservation ref immediately —
> containers are replaced and unpushed work is lost.

---

## 1. Project identity and philosophy

* Microkernel in Rust, `no_std` kernel crate (root `yarm`) plus workspace crates:
  `yarm-ipc-abi`, `yarm-srv-common`, `yarm-kernel`, `yarm-control-plane-servers`,
  `yarm-fs-servers`, `yarm-network-servers`, `yarm-driver-servers`, `yarm-ui-servers`,
  `yarm-compat-servers`, `yarm-runtime-tools`, `yarm-server-runtime`,
  `yarm-freestanding-alloc`, `yarm-user-rt` [VC, `Cargo.toml`]. Default cargo feature is
  `hosted-dev` (hosted model of the kernel used by unit tests) [VC].
* Mechanism vs policy: no service policy in the kernel; `src/services/*` must not return;
  boundary enforced by `scripts/check-crate-graph-boundary.py` and `phase5-boundary-gates` [VH].
* Capability authority moves only through the IPC transferred-cap path; cap ids are
  cspace-local; reply caps are one-shot, direct-only, non-delegatable [VH, `doc/AI_AGENT_RULES.md` §1].
* Three ISAs from day one; QEMU is the verification platform; RPi5 is diagnostic only [VH].
* Evidence culture: named witness markers, artifact-bound boots, graders that reject false
  evidence, predeclared qualification schedules, negative controls. The owner treats evidence
  defects as seriously as kernel defects [VH].

## 2. Verified repository state (2026-11-05)

* `main` = `claude/yarm-kernel-unlock-u0-f2zh8p` = `wip/kthread-closure1` = **`b81e0317`**
  (KTHREAD-CLOSURE1 Stage D). Stage refs: `wip/kthread-closure1-a` `136e8ae2`, `-b` `a540aef2`,
  `-c` `fc429b00`. `14b61286` (POST-U9-STABILIZATION, hardware bring-up reference) preserved on
  `wip/post-u9-stabilization`. 146 branches, 0 tags. **No newer work existed on any branch on
  2026-11-05** [VH]; the only later ref is the documentation branch
  `wip/yarm-context-recovery-2026-11-05` (`cce15d92`); `main` was still `b81e0317` on 2026-11-09
  [VH]. Re-check before acting.
* The local checkout at `/home/user/YARM` may be **shallow and on an older branch** (it was on
  `wip/kthread-closure1-a` during recovery). Always fetch and work from `origin/main`. [VH]
* Last qualified results (KTHREAD-CLOSURE1 Stage D, tree code-identical to `b81e0317`): hosted lib
  6 275 passed / 0 failed / 3 ignored; 41 integration binaries; KT1 witness at 1/2/4/8 CPUs on
  three ISAs; 40/40 AArch64 SMP2 boots; 9/9 stack-overflow probes; device interrupts on kernel
  threads on three ports; default images byte-identical to Stage C run 2. [VH]

## 3. Essential technical invariants (do not break)

From `doc/KERNEL_UNLOCKING.md` §3–§4, `doc/STATUS.md` §6, `doc/AI_AGENT_RULES.md` §6, contract docs:

1. **No runtime global kernel lock**, no renamed equivalent, no unrestricted `&mut KernelState`
   closure; ranked locks in ascending order only; the census guard must stay green. [VC/VH]
2. Every state transition through its single owner; identities carry generations; a stale
   token can never release a replacement; rollback restores generation and slot. [VH]
3. A VM change is complete only when its invalidation is; PTE removal → TLB shootdown → reclaim;
   an unacknowledged invalidation is a failure; every shootdown names its executing CPU; an
   address space whose root another CPU still has loaded is quarantined (permanently). [VH]
4. Frozen ABIs: SpawnV5 (16-byte reply), image ids 7–12, `SYSCALL_COUNT = 32` (public surface
   `0..=16` after `ExitCurrentTask` NR 16), `STARTUP_SLOT_COUNT = 18`, RecvSharedV3 offsets,
   `Message::MAX_PAYLOAD = 128`, optional-FS smoke marker names. [VH]
5. No `ipc_recv_with_deadline(_, 0)` in required-reply paths; PM-private reply endpoint
   isolation; `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`. [VH]
6. Initramfs data is read-only; only full non-writable pages map zero-copy; W+X segments
   rejected; child tasks never get MemoryObject caps; NR 27 is removed (zero-copy loading is
   mandatory). [VH]
7. Kernel uses no FP/SIMD outside audited init and context save/restore; eager user FP
   preservation and exact continuation-owner authentication unchanged. [VH]
8. Kernel-thread stacks run in a guarded window; a new address-space root must carry every
   mapping the kernel uses under it (the RISC-V device window bug). [VH]
9. Scheduler semantics (contract): wakes never preempt; quantum preemption at selection points;
   enqueue on the task's home CPU; `QueueFull` surfaced; no migration after placement for
   pinned plans by discipline. [VH, `doc/SCHEDULING_CONTRACT.md`]
10. Hosted tests are single-threaded; parallel runs show 58–71 known shared-state failures. [VH]
11. Endpoint receive waiters: last-receiver-wins; a displaced incarnation is ended through
    CLAIM(Teardown) → CANCEL → RETIRE before the new one is armed; a publication over a live
    claim is refused with zero mutation (`WaiterOwnershipBusy`); never weaken `arm_current`;
    every claim settles on every arm (`let _ = claim;` is forbidden by a source census). [VC]
12. A split transaction carries its executing/requesting CPU explicitly; never use the
    process-global `sched.current_cpu` as the executing CPU, and never "fix" that by rebinding it
    (it moves the race). [VH, Msgs 0366, 1089]
13. `Complete(Ok(()))` / `Complete(Err)` at a trap bridge returns **through the entering frame**;
    it is only correct when that exact task still owns the frame. [VH, Msgs 0730, 0907, 0932]

## 4. Subsystem status

Format: implemented / contract / compromise / risk / next. Unverified items are marked.

| # | Subsystem | Status |
|---|---|---|
| 1 | Microkernel split | **Impl:** kernel mechanisms + userspace service crates [VH]. **Risk:** policy creeping into kernel fixes. |
| 2 | Cross-arch | x86_64 PVH, AArch64 virt (GICv2), RISC-V64 OpenSBI; all three boot the core service chain and pass strict core smokes [VH]. **Compromise:** D6-genuine authoritative dispatch is x86-only [VC]. **RPi5:** diagnostic only (`rpi5-highhalf`). HH-5 bridge implemented 2026-06-21/22 — high-half FDT `/chosen` walker, high-half boot frame allocator and kernel-entry shim, BOOT-4 high-half kernel heap, gated phys↔virt direct map, `.bss` zeroing fix (`1e62310a`, `bbe6b6c7`, `7eb743de`, `ec1a063d`, `d1374f9a`) [VH]; enter-user markers exist in source [VC]; **hardware outcome after 06-22 is [UNK]** (the last commit reports an artifact build only); `doc/RPI5_BRINGUP.md` (last edited 06-15) and `doc/STATUS.md` still describe the pre-bridge deferral. |
| 3 | Boot / SMP bring-up | BootExclusive single-owner boot, published before first user entry [VC]; production AP admission with `PRODUCTION_CPU_CEILING = 8` on all three ports (`src/arch/x86_cpu_admission.rs:36`, `aarch64_cpu_admission.rs:45`, `riscv_hart_admission.rs:49`) [VC — corrected 2026-11-09; the 2026-11-05 text said no x86 constant existed]; x86 admission via an APIC-map bitmap [VC]; `MAX_CPUS = 64` [VC]. **Risk:** per-CPU setup mismatches (AP `EFER.NXE` history); RISC-V trap stacks scale per CPU (Msg 1117). |
| 4 | Kernel threads, scheduler, switching | `PriorityScheduler`, GP levels with quantum round-robin; per-CPU scheduler lock (SCHED-LOCK1); **non-preemptive kernel**; time = count of clock-owner timer interrupts (drifting, uncalibrated); `yarm_log!` flushes synchronously inside masked traps [VH, contract §1]. Kernel threads: cooperative, bounded, guarded, **no production user** [VC]. **Next:** RT-SCHED1. |
| 5 | Synchronization / unlocking | broad lock deleted; ranked locks; scheduler lock is an unfair TTAS spinlock waited on with interrupts masked (SC-LOCK-2 progress PARTIAL) [VH]. Canonical stages not re-classified [VH]. |
| 6 | IPC | send/recv/recv-v2/call/reply, shared-region transfers (offset + lifetime contracts), blocking with timeouts (off-lock pipeline on all arches; deadline store only 4 slots [VC]); direct NR6/7 production on all arches [VC]. **Open:** 199D ServerDies cells AArch64/RISC-V; 199F notification parity not qualified as a stage. |
| 7 | Waiters / wake arbitration / teardown | generation-bearing endpoint waiter records, central removal (WA3C1 `10b91677`), authoritative waiter ownership with last-receiver-wins displacement and live-claim refusal (WA3C2 `612339b5`, 08-31) [VC], notification waiters as exact `{tid, asid, generation}` records (U9-IRQ-UNKNOWN1) [VC], terminal arbitration for reply vs timeout vs peer death (200A–C), lossless parked server-death handoff (AP-SCHED4) [VH]. **Risk:** any direct clear of waiter state. **Open [INF]:** a displaced untimed receiver's *task* is not woken or re-checked by the publish path and no test covers its liveness. |
| 8 | Task / process lifecycle | SpawnV5, spawn provisioner with exact rollback, fork via one COW clone owner, reap and exit transactions (U9-*) [VH]. **Open:** RISC-V `ExitCurrentTask` live cell. |
| 9 | Memory / VM / COW | per-port page tables and ASIDs; MemoryObjects (Anonymous, InitramfsFileSlice); COW fork; typed invalidation completion; x86 IPI shootdown + ACK [VH], RISC-V SBI remote fence [VH], AArch64 inner-shareable broadcast TLBI (`tlbi vaae1is/aside1is/vmalle1is`, `src/arch/aarch64/page_table.rs`) [VC]. **Open:** canonical 201A–G not re-classified. |
| 10 | Capabilities | CNodes with generation-protected slots; one-shot direct reply caps; transfer materialization with rollback [VH]. **Open:** canonical 200A–D; several cap seams historically `HELPER_ONLY` [VH — current wiring not re-audited here, UNK]. |
| 11 | Userspace services | init_server, supervisor, process_manager (reply-cap call/reply helpers), initramfs_srv, devfs_srv, vfs_server [VH, STATUS §2 as of 10-13]. |
| 12 | Drivers / IRQ / DMA / virtio | driver_manager, blkcache_srv, virtio_blk_srv live in QEMU; device-interrupt witnesses (UART/PL011/COM1) live on three ports and on kernel threads [VH]. **Security gap [VC]:** `bind_irq_notification` (`src/kernel/boot/ipc_state.rs:7465`) requires only the Notification's SIGNAL right — no authority over the IRQ line; today it is reachable only from the default-off UART witness (`mod.rs:8750`), so it must gain an IRQ-capability check before any production IRQ-grant path (Msg 0688). Production driver control was `NoopDriverControl` and IRQMUX fake-only on 09-03 (Msg 0688) — not re-verified since. Recorded post-unlock driver ideas: userspace GPIO lease broker for shared lines above the RP1 server (Msg 0738). DMA: **[UNK]**. |
| 13 | Filesystems / VFS | VFS is implemented: `vfs_server` (mount/fd tables, frozen v1 opcodes) is live in the core chain; shared-memory VFS I/O exists as both direction helpers plus a RAMFS-only live route through `RecvV3SharedIoMapper` (Stages 63–87, 2026-06-05 → 06-10; `VFS_READ_SHARED_REPLY_ENABLED` and `VFS_WRITE_SHARED_REQUEST_ENABLED` are `true` in `crates/yarm-fs-servers/src/fs/common/shared_io_adapter.rs`), but the default `handle_request` still returns `Unsupported` for the shared opcodes, FAT live mount is disabled and ext4 is read-only [VC/VH]. Optional FS Milestone 1 (Stage 100) then paused FS work (`doc/AI_AGENT_RULES.md` §12.2). **Open:** a production `VfsSharedIoMapper` for the default dispatch (needs the FS-19 kernel ABI for authoritative object metadata, `doc/VFS.md` §5), FAT live mount, ext4 writes. **Correction note:** an earlier draft of this seed said the VFS roadmap had "not started" — that misread `doc/KERNEL_UNLOCKING.md` §0.8, which says VFS work was "not started **by SCHED-CONTRACT1**". |
| 14 | Errors / rollback / cleanup | transactional owners with failure tables; refusals settled from ownership; quarantine; no allocation in reap [VH]. **Risk pattern:** rollback gaps (see bugs ledger B9). |
| 15 | Testing / proof | hosted model + source guards + integration suites; QEMU witnesses with graders and negative controls; BUILD-GATES1 artifact binding; NOFP checker; stack-adjustment checker; census guard [VH]. **Missing:** 205B fault-injection matrix, 205C torture harness, any hardware timing evidence. |

## 5. Kernel-unlocking history in one page

Feb–May: one broad lock by design. June: Stage 101 restart; Milestone 1 (Stage 106, D1/D2/D5
splits), Milestone 2 (seams per rank, x86 AP online), x86 D6 switch-proof bring-up (117–132).
July: graduation of seams (181), fallbacks removed (182), SMP live (183), cross-arch live (184),
first-cohort cross-arch seal (197), NR 27 removed (197A), shared-region and reply-cap
transactions (198), direct IPC (199A), reply-timeout three-arch matrix (200C), server death and
NR 16 (200D). Aug: canonical 199D (ServerDies, waiter ownership, x86 direct default on → off →
on), U1/U2/U3 census 49 → 7, U4/U6/U7. Sep: U9 retired every terminal population and deleted
the last production acquisitions (`0dce4a7f`), U9 closed (`93900ae0`), `14b61286`. Late Sep –
Oct 8: QEMU-IRQ/SMP/LOCK evidence. Oct: BL backlog, shared-region lifetime, **broad lock deleted
(BLR1 `a75b1601`)**, VM-INVALIDATION1/2, **production SMP on three ports (AP-SCHED1–4)**,
scheduling contract, no-FP kernel, artifact gates, per-CPU scheduler lock (SCHED-LOCK1),
SMP3 evidence. Nov: kernel threads (KTHREAD1, -ACCEPTANCE) and KTHREAD-CLOSURE1 (`b81e0317`).
Full account: `doc/YARM_KERNEL_UNLOCK_HISTORY.md`.

**KTHREAD-CLOSURE1's five closures** [VH]: (1) KT1 replay refuses overlapping execution on one
CPU; (2) single-CPU timeout grading validates every sequence; (3) AArch64 SMP2 verifier race
(legal GICv2 SGI order; both verifiers repaired); (4) hardware stack protection for kernel
threads on three ISAs; (5) plain non-masking `SpinLock` held across a scheduling point is
detected and refused (SC-KT-8). Remaining limits: execution-stamp evidence is witness-only;
void TLB rounds self-reported; no stack-depth bound; image bound assumes compiled-code frames;
forgotten plain guards keep a thread ineligible; two plain-lock latches hosted-only; QEMU only.

## 6. Critical bugs and accepted fixes (top ten; full ledger in `doc/YARM_BUGS_AND_LESSONS.md`)

1. RISC-V kernel-thread root missing the device window → device IRQs on kernel threads broken (`bb451a3b`).
2. AArch64 overflow handling must leave the faulting stack before any store (emergency stack).
3. `destroy_endpoint` lease leak and stranded receiver (WA3C1 `10b91677`).
4. Spawn endpoint leak on `CapabilityFull`; index-as-identity (U9-SPAWN-IC1 `31c68896`).
5. Broad-lock regression introduced by a refactor (U9-DISPATCH-CPU2 `8265461b`).
6. x86 dispatch-gap TLB invalidation hole (VM-INVALIDATION2 `da591ee8`).
7. Unacknowledged invalidation reported as success (VM-INVALIDATION1 `a72c69a7`).
8. User-origin #GP / illegal instruction halted the kernel after the broad arm became unreachable (BL2).
9. Compiler-generated FP/SIMD in kernel code (KERNEL-NOFP1 `fec063af`).
10. Plain lock held across scheduling points undetected (KTHREAD-CLOSURE1 B `a540aef2`).

From the August–September record (bugs ledger A29–A49): the waiter-arbitration defects that
forced direct IPC off (bare-TID notification waiter, timeout before waiter cleanup, a scanner
cursor advanced by wakes), the ownership leaks `let _ = claim;`, ambient `sched.current_cpu` as
the executing CPU, a shared bridge that never saved the entering task's registers, AArch64
`x15` zeroed on EL0 return, kind-blind MemoryObject reclaim that freed initrd memory, fork and
address-space teardown without inverses, and exit settlements that resumed a frame no task owned.

## 7. Current blockers and unfinished work

1. Canonical stage re-classification against the post-deletion tree (owed; do stage by stage).
   Historical status and current relevance are separated in `doc/YARM_KERNEL_UNLOCK_HISTORY.md`
   ("The 35 canonical stages"): 204E is done in substance (BLR1) but unreviewed; the BSP-only
   clauses of 201F/203D and the re-acquisition clause of 204C are superseded; 204D scaffolding
   (`GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE`, `M2_SEAM_HELPER_ONLY`) is still in the tree; many
   VM/lifecycle stages were largely delivered by U9 directives and need re-qualification, not design.
2. SC-LOCK-2: fair kernel lock or contention bound and a progress argument.
3. D6-genuine authoritative dispatch only on x86 (203C).
4. Asynchronous kernel preemption (SC-KT-4); stack-depth bound; first production use of kernel threads.
5. ServerDies live cells on AArch64/RISC-V; `ExitCurrentTask` RISC-V cell; notification parity (199F).
6. Deadline store of 4 slots; per-arch deadline timebases not unified; time uncalibrated.
7. 205B/205C/205D harnesses and markers do not exist; parallel hosted-test contention.
8. Stale docs: `doc/STATUS.md` §1/§5, `doc/AI_AGENT_RULES.md` §3.4 and §6, `doc/KERNEL_UNLOCKING.md`
   §0.3 wording, a stale comment near `src/kernel/boot/mod.rs:4024`.
9. Hardware in the loop — prerequisite for any timing bound. RPi5: the HH-5 bridge code exists
   (June); its hardware result and the next blocker after BOOT-4 are unknown and must be
   re-established on hardware before planning (stale: `doc/RPI5_BRINGUP.md` §12–13).
10. VFS: production shared I/O in the default dispatch (FS-19 kernel ABI), FAT live mount, ext4 writes.
11. Displaced-receiver liveness under last-receiver-wins: untested (see subsystem 7) — a bounded
    test question, not a confirmed bug.
12. IRQ-line authority in `bind_irq_notification` (subsystem 12) — prerequisite for production
    userspace drivers.
13. Carried from POST-U9-STABILIZATION (Msg 0949), not re-verified since: the x86 ordinary-cap
    cell blocked by init's mapping capacity (`MAX_MAPPINGS` 128/128); AArch64/RISC-V server-death
    runners never live because no initramfs is supplied (harness gap). Later packages may have
    changed these — check `doc/KERNEL_UNLOCKING.md` before planning.
14. Owner-deferred features: futex requeue / `FUTEX_WAIT_BITSET` / PI futexes (Msgs 0339–0342),
    removal of historical x86 AP debug breadcrumbs (Msg 0113), RPi5 bare-metal bring-up.

## 8. Next exact implementation priority [INF recommendation; the owner decides]

The recorded roadmap names **RT-SCHED1** next (`doc/KERNEL_UNLOCKING.md` §0.8, contract
§18.10.4). Recommended order:

1. **A docs-only reconciliation package first** (no code): correct `doc/STATUS.md` §1/§5 to the
   AP-SCHED/BLR1/KTHREAD state, fix the stale `AI_AGENT_RULES` items and the stale comment, and
   add a "post-BLR1 canonical re-classification needed" note per open canonical stage, and
   refresh `doc/RPI5_BRINGUP.md` §12–13 and `doc/VFS.md` §5 to the June bridge / shared-I/O
   state — so the next agent does not plan from stale pages. Qualify with fmt, the doc guards and the census guard.
2. **RT-SCHED1, slice 1: the kernel-lock fairness / progress prerequisite (SC-LOCK-2)** — replace
   or bound the unfair TTAS rank-1 wait with a fair lock or a measured contention bound, with a
   progress argument, hosted model + mutation checks + three-port QEMU witness, no policy change.
3. Then RT-SCHED1's calibrated time, fixed-priority preemptive selection and budgets, in bounded
   slices, each qualified separately.

## 9. Rules future agents must obey

1. Read before writing: `doc/AI_AGENT_RULES.md`, `doc/KERNEL_UNLOCKING.md` §0 (canonical
   stages), §3 (invariants), §4 (fixes to preserve), `doc/SCHEDULING_CONTRACT.md`, the record of
   the last delivered package, and this seed.
2. Verify refs (`git fetch`; work from `origin/main`); verify every claim in current source.
3. Reproduce on the unmodified base; identify the first causal failure; repair the owner.
4. One bounded issue per delivery; HARD-STOP and record when no safe seam exists.
5. Never: global lock reintroduction, renamed wrappers, capacity increases, weakened guards,
   oracle-only fixes, ABI changes, unrelated cleanup, policy in the kernel.
6. Witnesses default-off; they observe production owners and never carry the repair.
7. Qualify on all three ports from a frozen worktree with fresh artifacts, predeclared schedule,
   negative controls; keep every result; never replace a failed scheduled run with a retry;
   disclose compile-only or unavailable-QEMU coverage.
8. Label every claim by level (hosted / live proof / production / canonical complete) and port.
9. Preserve `14b61286` and every ref; no force push, rebase, history rewrite, deletion or PR
   unless asked; deliver by fast-forward with ancestry and remote-equality checks; commit
   trailers as the owner specifies; no model identifiers in commits, code or docs.
10. Operational: never `pkill -f`; never edit tracked files during a boot; never edit a running
    script; watch disk; hosted tests `--test-threads=1`, `CARGO_INCREMENTAL=0`; witness lines
    ≤ 192 bytes.
11. Prompt discipline inherited from the owner's directive reset (Msg 0012): every package names
    its objective, the population or metric it changes (before → after), the exact owners or call
    sites, and its non-goals; a prerequisite with no metric change must name what it unblocks; no
    more than two such passes in a row; "a marker proves execution, not retirement".
12. Before declaring a gate unreachable, inventory every existing profile and read the code
    body, not its header (the WA3C2 false blocker, Msgs 0558/0560).
13. Commit (or use a separate worktree) before any base-comparison checkout; push green
    checkpoints to a preservation ref at once; after a container restart, distrust everything on
    disk until its date and commit are checked.
14. Never cite a number you did not observe in this session. The retracted "3525 tests / 31
    failures" (Msg 0001) is the cautionary example.

## 10. Where to look

| Need | Document |
|---|---|
| Canonical unlocking stages and every stage record | `doc/KERNEL_UNLOCKING.md` (§0 ladder; §0.8 post-unlock roadmap and Oct–Nov records; U9 records; BLR1/VMI1/APS1–4/VMI2 at the end) |
| Lock ranks | `doc/KERNEL_LOCKING.md` |
| Per-callsite evidence / syscall matrix | `doc/KERNEL_UNLOCK_AUDIT.md` |
| Scheduling contract, kernel threads (§18) | `doc/SCHEDULING_CONTRACT.md` |
| Milestones Phase 0–199D and seal ledger | `doc/PROJECT_HISTORY.md` |
| IPC / capabilities / spawn / VFS / FS | `doc/IPC.md`, `doc/CAPABILITY_MODEL.md`, `doc/PROCESS_AND_SPAWN.md`, `doc/VFS.md`, `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` |
| Per-arch | `doc/ARCH_X86_64.md`, `doc/ARCH_AARCH64.md`, `doc/ARCH_RISCV64.md`, `doc/RPI5_BRINGUP.md` |
| Test rules | `doc/KERNEL_TEST_RULES.md` |
| This recovery | `doc/YARM_DEVELOPMENT_HISTORY.md`, `doc/YARM_KERNEL_UNLOCK_HISTORY.md`, `doc/YARM_BUGS_AND_LESSONS.md`, `doc/YARM_RECOVERY_SOURCES.md` |
| Owner conversation × Git: U0–U9 ladder, 199C/199E/WA3C2 chronologies, stops, lost work, owner proposals | `doc/YARM_STAGE_LEDGER.md`; reconciliation report `doc/YARM_CHATGPT_RECONCILIATION.md` |
