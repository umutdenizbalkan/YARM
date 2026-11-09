<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM stage ledger — owner conversation × Git (2026-08-10 → 2026-11-04)

This ledger links the owner's original ChatGPT planning conversation ("YARM kernel unlock",
id `6a7a20ea-c638-83eb-8679-0715c202753b`, exported 2026-11-05/06) with the Git history. Every row
gives the **message number** in the export's chronological transcript
(`YARM_ORIGINAL_CONVERSATION.md`, "Message ####"; user turns are mostly Claude Code reports pasted
by the owner, assistant turns are ChatGPT's reviews and prompts) and the **commit(s)** concerned,
with their status in a full clone of `origin` taken 2026-11-09.

The raw export is **not** stored in this repository (owner instruction). Message numbers are
stable references into it.

## Evidence rules for this file

| Label | Meaning here |
|---|---|
| **MAIN** | commit is reachable from `origin/main` (full clone, 2026-11-09) — **[VH]** |
| **REF** | commit exists only on another remote ref (usually a `wip/*` preservation branch) — **[VH]** |
| **ABSENT** | the hash is not an object in the clone: a local checkpoint that was amended, never pushed, or lost with a container — **[VH]** that it never reached `origin` |
| **[SR]** | the outcome is as the report describes it; reports are Claude Code's own words relayed by the owner and are **not** independent verification |
| **[VC]** | re-checked in source at `b81e0317` |

A report saying "delivered" is only [SR] until a MAIN/REF commit backs it. A report saying "not
pushed" is corroborated when the hash is ABSENT. Tree hashes quoted in reports are omitted.

Coverage: the export's index lists 310 substantive reports and directives among 436 owner turns;
the rows below are the curated subset needed to reconstruct decisions, failures and deliveries.
A full mechanical 310-row table was generated during reconciliation (classifier counts: 155
delivered, 97 not delivered or stopped, 16 WIP-pushed, 7 local checkpoints, 3 lost-work, the rest
diagnosis/in-progress/owner turns), but the scratch copy was destroyed by a container restart
on 2026-11-09 together with the uploaded export; it can be regenerated from the export.

---

## 1. The U0–U9 directive ladder

Origin: on 2026-08-10 the owner uploaded two files, `yarm_unlocking_directives.txt` and
`yarm_microkernel_review_cycle14.txt` (attachments; their text is **not** in the export — known
only through ChatGPT's paraphrase, Msgs 0010–0012, 0684, 0792), after objecting that the planning
kept "drift[ing] from kernel unlocking to bug fixing" (Msg 0008). Msg 0012 re-based the census on
source (**49 acquisitions = 39 `with_cpu` + 10 broad `with`; 44 runtime / 3 test-only /
2 obsolete**, superseding the seed's historical 51/46, Msg 0001) and fixed the ladder:

| Directive | Definition (Msg 0012) | Outcome | Evidence |
|---|---|---|---|
| **U0** | fix the map: reconcile `KERNEL_UNLOCKING.md` §0 with source; delta 0 | delivered 08-11 | `03f6e5bd` MAIN (Msg 0025) |
| **U1** | delete obsolete sites, 49 → 47 | delivered 08-11 | `26364834` MAIN (Msg 0028) |
| **U2** | remove test-only sites from the production census, 47 → 44 | delivered 08-11 | `7bb73d85` MAIN (Msg 0040) |
| **U3** | make existing scheduler/task seams authoritative and delete their drains; exit = census ≤ 24 **and** no drain re-acquisition | ≤ 24 reached 08-12 (`4336c44a`, Msg 0119) but drains remained; exit met 08-24 at 7 | first `b7d4dcf8` (44 → 43, Msg 0043) … last `0912a279` (8 → 7, Msg 0400), all MAIN; status call Msg 0395 |
| **U4** | cross-architecture queue-advancing dispatch | delivered 08-13; blocking receive live on 3 ports, blocking-send origins hosted-proven only (QEMU 0/3) | `4f74f55b`, `c817ab66` MAIN (Msgs 0161, 0164) |
| **U5** | wire one existing capability seam (no fifth seam) | **no code**: exit found already satisfied — `materialize_received_message_cap_routed_with_delegation_split` had two production callers (Msgs 0166, 0169); canonical 200B stays open | [VH by report + source]; `M2_SEAM_HELPER_ONLY` fences remain [VC] |
| **U6** | retire blocking `IpcSend` waiter publication (must delete call sites) | partial 08-14; "199C COMPLETE" 08-14; re-opened 08-19; deferred 08-24; closed 08-24 by U6-FRAME | see §2 |
| **U7** | retire `IpcSend`/`IpcCall` timeouts | pipeline 08-17; canonical 199E closed 08-19 after AArch64/RISC-V tick work | see §3 |
| **U8** | localize the AArch64 split return | **already complete on 08-10** ("site already retired — do not reopen", Msg 0012) | Msg 0012; see the U8 contradiction in §6 |
| **U9** | remove fallbacks, scaffolding, then the lock | census 7 → 6 (`fbe1229c`, 08-24) → 2 (`d741f88b`, 08-25) → 0 (`0dce4a7f`, 09-24); closed `93900ae0` | Msgs 0428, 0466, 0928, 0936, 0942 |

Rules fixed with the ladder (Msg 0012) and repeated in every prompt: name the directive, the
canonical stage, CENSUS BEFORE/AFTER and the exact call sites; a bug is eligible only as
`BLOCKS: U<n>` and is fixed minimally; at most two consecutive delta-zero passes; no new oracle,
seal, script, marker or helper-only seam unless indispensable; "markers prove execution, not
retirement"; verification must finish before delivery. **WA3C2 was frozen** as an independent
objective (resumable only if a named U-call-site needed it). Canonical stages then stood at
**1 complete (204A) / 12 partial / 22 open**.

Why the priorities moved afterwards:

* **Batching for cost** — owner, Msgs 0046, 0155, 0321, 0396 ("do as much as possible in one
  pass"). Single-call-site passes gave way to cohorts.
* **Census ≠ effort** — at census 2 each remaining acquisition multiplexed ~18 syscall classes
  (Msg 0557). WA3C2 was **re-authorized** (Msg 0551, 08-27) because direct `IpcCall`/`IpcReply`
  could never leave the terminal dispatcher while direct production was OFF.
* **Operational drift** — owner checks at Msgs 0636, 0682, 0784. ChatGPT's answers: "not
  architectural drift, but … restore roadmap discipline" (Msg 0637); the 2026-09-08 re-baseline
  (Msg 0792) found four syscalls with **no split route at all** (NR 3, 4, 13, 30), preempting
  timers still declining the split route, NR 6 falling back without a claimable ack and NR 1 on a
  user-copy failure, and stated the rule "deliver a named reduction in the remaining dispatcher
  populations".
* **Owner sequencing** — finish unlocking, then bare-metal RPi5 and futexes (Msg 0396, 0689);
  futex requeue/bitset/PI deferred until unlocking ends (Msgs 0339, 0342); after U9, a bounded
  stabilization then QEMU-first validation before any hardware (Msgs 0944–0955); owner proposed
  physically deleting the broad lock (Msg 1073) → BLR1; owner asked whether to remove the shared
  scheduler lock (Msg 1171) → SCHED-LOCK1 added as a prerequisite before preemptible kernel
  threads; owner bundled the five open items into KTHREAD-CLOSURE1 (Msg 1236).

Estimates versus outcome: 9–14 passes to full unlock (Msg 0521, 08-26), 6–10 (Msg 0557, 08-27),
8–12 (Msg 0637, 09-01). U9 actually closed 09-24 after roughly a hundred more reports
(Msgs 0424 → 0942). [VH for the dates; the estimates are ChatGPT's]

---

## 2. U6 / canonical 199C — four status changes

| Msg | Date | Event | Commit |
|---|---|---|---|
| 0169, 0173 | 08-13 | stopped clean before editing; §2 analysis changes the design | — |
| 0176 | 08-13 | local checkpoint, includes AArch64 ELR repair (vector stores `elr_el1` verbatim; stale `+4` case removed) | `fcf42a6` ABSENT (amended) |
| 0180 | 08-14 | **partial cohort**, "199C OPEN/PARTIAL" | `b83ae3a8` MAIN |
| 0183 → 0188 | 08-14 | shared-region checkpoint, then **"U6 DELIVERED; 199C COMPLETE"** — blocking-send QEMU coverage "honestly 0/3" | `13bb5261` ABSENT; `dfaaa648` MAIN |
| 0319 | 08-19 | docs pass qualifies §0.7's "199C COMPLETE" → **199C OPEN** per §0.3/§0.4; arithmetic corrected to 2/11/22 | `3943472d` MAIN |
| 0402 → 0414 | 08-24 | U6-RW hard stops: x86 causal defect found and reverted, RISC-V does not qualify, premise disproven | none (tree clean at `0912a279`) |
| 0416 | 08-24 | U6-V2 stop: **199C declared deferred** (needs a new syscall lane or unrelated redesign) | none |
| 0418 | 08-24 | **U6-FRAME: 199C DELIVERED/CLOSED** — one canonical message envelope preserved through endpoint refill; arithmetic 2/11/22 → **3/11/21** | `b59b7225` MAIN |

## 3. U7 / canonical 199E — from pipeline to closure

| Msg | Date | Event | Commit |
|---|---|---|---|
| 0194 | 08-17 | U7 delivered: production timeout pipeline wired on three arches; but default reachability unproven on AArch64/RISC-V (non-advancing `scheduler_tick_now()`), so 199E stays open (Msg 0195) | `8275c927` MAIN |
| 0202 → 0252 | 08-17 | eight local amends of one checkpoint: bare-TID retirement strengthened; six test migrations; live-proof attempt **reverted** (Msg 0225); a feature-on kernel "died early" — first blamed on the oracle, then on `KERNEL_DEBUG_ELF` (wrong, Msg 0234), finally `--profile dev`-specific (Msg 0237); gates failed (Msg 0243); a reclassification exposed a semantic runtime failure (Msg 0246); mixed reply-deadline clock domains fixed with per-record `ReplyDeadlineClock` (Msg 0249) | `2e0739c`, `f4cea0b`, `70559f7`, `b5d6cba`, `079bf4c`, `bfda163` ABSENT |
| 0255 | 08-17 | 199E checkpoint delivered (clock domains, oracle accounting) | `90a52dc8` MAIN |
| 0266 → 0274 | 08-17 | AArch64 timer bring-up failed closed; root cause: DTB walker chose the GIC CPU-interface `reg` tuple before reading `compatible`, so the distributor base was used as GICC — dormant only because AArch64 had never taken an interrupt | `6dfbf69` ABSENT → `9f0e4896` MAIN |
| 0277 → 0283 | 08-18 | timer armed but zero IRQs late in boot: `copy_bootstrap_kernel_root_entries` skips L1[0] (1 GiB containing GIC and UART); UART survived only via an explicit per-root leaf | `7fe7d066` MAIN |
| 0286 → 0291 | 08-18 | AArch64 ProductionTick live but regressed the core gate (supervisor 1-tick policy); repaired | `1d8a49d`, `c8491ec` ABSENT → `92a99596` MAIN |
| 0293 → 0307 | 08-18 | RISC-V S-mode timer bridge: D2 receive-return provenance hole (`RISCV_BLOCKED_IDLE_NO_PROVENANCE`) and cumulative `sscratch` drift one frame per trap (Msg 0294: "a 2 MiB stack only postpones exhaustion"); U-mode-origin proof blocked by `scounteren`; a circular timer/idle dependency (Msg 0301); asynchronous-preemption register convention added | `66ec248`, `dea428c`, `32ba4f3`, `aa0bfd8`, `f3eb249` ABSENT → `afdc9927` MAIN |
| 0309 → 0311 | 08-18 | user FP had been "**safe by accident**" (user target advertised lp64d and OpenSBI left `sstatus.FS` Dirty); fail-closed RV64IMAC/LP64 policy with FS/VS forced Off on every U-mode return | `7e39333` ABSENT → `932bd6fa` MAIN |
| 0313 → 0317 | 08-19 | default timer admission red; WIP preserved instead of squashing to `main` | `c521588`, `ea16b3e`, `e46d936` REF (`wip/199e-riscv-default-timer`) |
| 0319 | 08-19 | 199E promoted CLOSED; `main` at the RISC-V ProductionTick-default commit | `e019c8aa` MAIN |

## 4. WA3C2 — proposed, rejected, deferred, delivered

| Msg | Date | Event |
|---|---|---|
| 0001 §11 | before 08-10 | the first WA3C spec (record + ARM + **strict single-waiter publication** + retirement + unwind in one increment) was **stopped and reverted**. Verified evidence: `vfs_file_grant_ro_relay_preserves_transferred_cap` passes under last-receiver-wins and **hangs** under strict publication; six named replacement tests and a 35-test differential family encode replacement. A reported "3525 tests completed, 31 failures" was **fabricated, never observed, and retracted** — never cite it. |
| 0001 §17 | | the WA2A ownership table holds one exact `WaiterKey` per endpoint and `arm_current` refuses an occupied slot, so replacement needs an explicit atomic ownership transition; "do not weaken `arm_current`". |
| 0007 | 08-10 | ChatGPT (design-only, no build) proposed adopting strict publication anyway, arguing the hang came from a missing Phase-C unwind (refusal after scheduler/TCB were already blocked) and that a displaced untimed receiver "can remain stranded forever". **Not accepted.** |
| 0008, 0012 | 08-10 | owner redirect to unlocking; WA3C2 frozen. |
| 0481 | 08-26 | still deferred; direct production OFF. |
| 0551, 0554 | 08-27 | re-authorized as the 199D/U9 blocker. Directive: "Do not retry strict single-waiter publication merely because it appears simpler — the earlier strict attempt hung the relay … Otherwise retain last-receiver-wins with an explicit displacement ownership transition"; "displacement cannot leave the old task blocked without an owner". |
| 0558 | 08-27 | Claude stopped: SMP=2 gate "unreachable"; located the displacement defect in `publish_recv_waiter_locked` (logged and dropped the displaced record; old receiver blocked, ownerless); all six ownership operations had 0 production callers. |
| 0560 | 08-27 | ChatGPT: the blocker was false — the wrong (older, self-declared "blocked") SMP profile had been used; three existing `ap-cross-cpu-*` profiles already prove cross-CPU direct call/reply. |
| 0561 | 08-31 | **delivered `612339b5` MAIN** after a container replacement destroyed the unpushed commits; rebuilt from scratch and re-measured. |

Delivered semantics [VH `612339b5` message; VC `src/kernel/boot/ipc_state.rs:1971-2000`,
`src/kernel/boot/waiter_ownership.rs:971-1021`]: last-receiver-wins **preserved**; a displaced
incarnation goes CLAIM(`WaiterOwner::Teardown`) → CANCEL → RETIRE before the new one is ARMed; a
publication that would take an endpoint from under a **live claim** is refused with zero mutation
(`PublishWaiterOutcome::WaiterOwnershipBusy` → `WouldBlock`); `let _ = claim;` leaks on request
`ServerGone` and reply `CallerGone` now CANCEL; production default `cfg!(target_arch = "x86_64")`,
unconfined to the oracle endpoint; the split blocking-`IpcRecv` route kept alive by sharing the
ack-publication bodies. All three arches became production-default on 09-02 (`2aab8b77`
DIRECT3-CAP-FINAL, Msg 0653) [VH, VC `mod.rs:4154`].

**Open question [INF], not established either way:** the repair ends the displaced incarnation's
*ownership*; neither `publish_endpoint_waiter` nor its tests
(`a_displacement_retires_the_departing_and_arms_the_arriving`) wake or re-check the displaced
*task*. Whether an untimed displaced receiver can stay `Blocked` indefinitely (Msg 0007's
liveness concern) depends on whether two receivers ever wait on one endpoint in production; no
test or record answers it.

## 5. Chronological ledger (curated)

### August

| Msg | Date | Stage | Outcome | Commit(s) |
|---|---|---|---|---|
| 0001 | 08-10 | owner seed after 199D-WA3C1 | head accepted | `10b91677` MAIN |
| 0019 | 08-11 | preflight | **stopped**: checked-out branch ≠ `main` | — |
| 0022 | 08-11 | branch alignment (owner merged `claude/kernel-unlock-audit-i7nt1t`, Msg 0016) | delivered | `e4e70618` MAIN (PR #542 merge) |
| 0025/28/40 | 08-11 | U0, U1, U2 | delivered | `03f6e5bd`, `26364834`, `7bb73d85` MAIN |
| 0034, 0037 | 08-11 | QEMU installed for three ISAs (Msg 0031); first matrix 3 of 4 attempted | partial / pass | — |
| 0043, 0048 | 08-11 | U3 RISC-V drains 44 → 42 | delivered | `b7d4dcf8`, `09bfee51` MAIN |
| 0052, 0059 | 08-11 | U3 trap-path cohorts | **stopped before editing — capacity, not a technical blocker** | — |
| 0054 | 08-11 | three RISC-V drains | delivered | `a1d55115` MAIN |
| 0062 → 0076 | 08-11 | AArch64 trap_entry cohort | **not delivered**: four live gates failed; diagnosed as init workload selection / late control-flow placement; blocker fix | `6001627a` MAIN (later), `7504683f` ABSENT |
| 0078 | 08-11 | **reduced cohort**: two live-proven drains retired, unverified idle drain restored (39 → 37) | delivered | `0adf6187` MAIN |
| 0082 | 08-11 | target path B not reached at base | stopped | — |
| 0085–0099 | 08-11/12 | RISC-V exit validation, x86 D2/futex/yield restores (37 → 28) | delivered | `4170bb39`, `0eac8ab9`, `f63e7089`, `3b243e79` MAIN |
| 0103 → 0109 | 08-12 | x86 current-identity cohort | **not delivered** ×3: intermittent stress-gate failures; the agent's first characterization was wrong (it had grepped only for the noise it expected) | `c4f58ca1` (unpushed then) |
| 0115 | 08-12 | root cause: five late AP naked-asm breadcrumbs writing COM1 while the BSP logged; moved to debugcon 0xE9 (28 → 25) | delivered | `f7e25f29` MAIN |
| 0119–0130 | 08-12/13 | x86 saved-context, ED-1, block-current, D6 first-resume (25 → 21; ED-2 excluded as unreachable) | delivered (reduced) | `4336c44a`, `cbf7ec3e`, `57b8be10`, `1bf445dc` MAIN |
| 0133, 0139 | 08-13 | validation body not live-reached; cap-rollback drain needs a capability-model redesign | stopped (semantic blocker) | — |
| 0136–0158 | 08-13 | blocked-waiter phase C, idle revalidation, copy-fault, terminal idle, deferred sender wake (21 → 13) | delivered | `a642528c`, `760527d0`, `f3dd6808`, `9d82e923`, `b0ba244b` MAIN |
| 0161, 0164 | 08-13 | U4 | delivered | `4f74f55b`, `c817ab66` MAIN |
| 0169 → 0188 | 08-13/14 | U6 | see §2 | |
| 0191 | 08-14 | preflight item mismatch | stopped | — |
| 0194 → 0319 | 08-17 → 19 | U7/199E | see §3 | |
| 0319 | 08-19 | AArch64 `CurrentTaskExited` validation (13 → 12); docs: "U8 is next" removed, 199C re-opened, 200B/U5 restated | delivered | `3943472d` MAIN |
| 0324, 0330 | 08-19 | derivation stop; AArch64-only cohort local | stopped / local | `29391d7` ABSENT |
| 0334, 0348 | 08-19 | AArch64 exit-restore, FutexWait idle (12 → 10) | delivered | `6dd3ca4e`, `bad2ae33` MAIN |
| 0352 → 0373 | 08-19 | x86 SMP direct-reply chain: reachability gate failed; CPU-1 server `IpcRecv` resolved the wrong CNode (`MissingRight`); then `X86_BSP_SAVED_DISPATCH_OK` still 0 — root cause **process-global `sched.current_cpu` used as the executing CPU** | four stops, local `b070656` ABSENT | — |
| 0377 | 08-19 | `current_tid_authoritative(cpu)` without binding ambient CPU | delivered | `c2b9b779` MAIN |
| 0382, 0386 | 08-19/20 | 10 → 8 | local `2ed7124` ABSENT → delivered | `39c75dc3` MAIN |
| 0391, 0400 | 08-24 | ED-2 placement (8 → 7); **U3 exit met** | local `0d5ed133` ABSENT → delivered | `0912a279` MAIN |
| 0402 → 0418 | 08-24 | U6-RW/V2/FRAME | see §2 | `b59b7225` MAIN |
| 0422, 0424 | 08-24 | combined seal **truncated by the agent's own 50-minute timeout** (6 of 7 cells); finished and base-compared before being called qualified; U9-A inventory | delivered | `4e9643fd` MAIN |
| 0426 | 08-24 | U9-B: "STOP — the four-site cohort is two fences, not one" (docs-only push) | stopped | `c6a4a7db` MAIN |
| 0428 | 08-24 | U9-C reply-cap Phase-A materialization (7 → 6) | delivered | `fbe1229c` MAIN |
| 0432, 0434 | 08-24 | U9-D red; U9-E blocked — and a correction: a boot-time `X86_SMP_PLACEMENT_GATED` marker had been misread as a steady-state gate | stopped | — |
| 0436 → 0442 | 08-24/25 | U9-F capability-revoke teardown split per class | first build `f0f9890` **lost** with a container; rebuilt (owner chose "Rebuild U9-F at `fbe1229`", Msg 0441: "never claim equivalence with the lost `f0f9890`") | `2dbb751`, `f0f9890` ABSENT → `e9a7750c` MAIN |
| 0444 → 0450 | 08-25 | four stops (shootdown entry point, IPI asm feasibility, premise not in source) | stopped | — |
| 0454 | 08-25 | **container replaced; all U9/203C/204D work of the turn lost** (nothing committed by directive) | lost | — |
| 0456 → 0460 | 08-25 | rebuilt: post-switch restore leaves the broad lock (owner chose option 1, Msg 0452) | delivered | `7d2efec9` (checkpoint), `d3032f8e` MAIN |
| 0464, 0466 | 08-25 | **U9-D3: census 6 → 2**; the agent fixed its own 20 000 000-iteration CPL3 proof spin before delivery | local `05bb22b` ABSENT → delivered | `d741f88b` MAIN |
| 0468, 0470 | 08-25 | both terminal dispatchers unretirable under the directive (no off-lock queue-advancing switch from a pre-lock route) | stopped | — |
| 0472 → 0494 | 08-25/26 | U9-QA in WIP-1…WIP-10 | WIP pushed, then delivered | `2f1cbb5`, `ba53f1d`, `34df5f6`, `e88f19f` REF; `6a21e07b` MAIN |
| 0498 | 08-26 | TM1 — correction: the timeout collector named as a blocker at WIP-9 owned zero timeout classes (body never read) | WIP | `66e3219` REF |
| 0500, 0502 | 08-26 | container re-provisioned: uncommitted doc edits lost and rewritten; re-verification caught three wrong statements | delivered | `adcf229` MAIN |
| 0506 → 0532 | 08-26 | U9-PF, -FT1..3, fault drain, -RX..-RX4 | delivered (several as foundations, not cohorts) | `f5c6a9a`, `fb30b74`, `3132cac`, `f415024`, `d0d0a1e`, `d2f27dc`, `81172ac`, `65a144c`, `11d6ba4` MAIN |
| 0528 | 08-26 | correction: a base-comparison checkout silently reverted 8 uncommitted guards, so `d2f27dc` shipped without them; rule adopted: commit before any base checkout | delivered | `81172ac` MAIN |
| 0535, 0537 | 08-26 | U9-COW1 premise false (VM_COW witness unreachable); COW2 restores the witness | stopped / delivered | `894cc5a1` MAIN |
| 0545 | 08-26 | container re-provisioned; an already-posted cross-arch comparison came from stale Aug-24 logs in the fresh checkout — retracted | recovered | — |
| 0547, 0550 | 08-27 | residual matrix; census stays 2 | stopped | — |
| 0558, 0561 | 08-27/31 | WA3C2 | see §4 | `612339b5` MAIN |
| 0564 → 0583 | 08-31 | NR 5 / NR 1 routes: hard stops; one x86 blocker was an admission screen asking the wrong question (`kernel_context.initialized`) | WIP / delivered | `56802878` MAIN; `5d1dd2e`, `3b9c861`, `9ae31b7`, `7f3e3ff` REF |
| 0570 | 08-31 | container replaced again; pushed checkpoints survived | recovered | `583e682` REF |

### September

| Msg | Date | Stage | Outcome | Commit(s) |
|---|---|---|---|---|
| 0585 → 0605 | 09-01 | NR 1 off both terminal dispatchers; defects: shared bridge never saved the entering task's registers (Msg 0597); AArch64 `REG_X18_TLS` was lane 15, zeroing user `x15` on every EL0 return (Msg 0602) | not delivered ×5, then delivered | `c399e13` REF; `a3aa7cb6` MAIN |
| 0614 | 09-01 | **self-caused regression**: reply-terminal arming moved 41 direct NR 7 transactions back to the broad dispatcher on x86 | stopped | — |
| 0620 | 09-01 | **self-correction**: "broad NR 7 = 1" was a miscount (declines before the claim emit no marker); real ledger 41/10/1/2 of 54 | stopped | `cf400b3` MAIN |
| 0623 → 0653 | 09-01/02 | DIRECT3 queue/cap: record reuse, reply-cap ledger, oracle repaired (claimant-aware) not the arbitration | WIP → delivered; **NR 6/7 production default on all three arches** | `800e22f`, `6014ab9`, `6fa137f`, `c84f58f` MAIN; `2aab8b77` MAIN |
| 0656, 0659 | 09-02 | U9 final close / MO create | stopped at §1 | — |
| 0662 | 09-02 | U9-MO2: MemoryObject kind-blind reclaim (`free_frame(object.phys)`) under-freed Anonymous and **corrupted the allocator** for initramfs slices; Stage 191C had named the wrong property | delivered | `14db9ee` MAIN |
| 0665 | 09-02 | U9-SPAWN1 SP-1: one task-enqueue owner (eleven transcriptions, already drifted) | delivered | `8b4e1b2` MAIN (`c750af5`, `3a0df6a` also MAIN) |
| 0668 | 09-02 | U9-ASPACE1: teardown leaked one frame per ELF page; NR 26 removed | delivered | `63b17065` MAIN |
| 0673 | 09-03 | U9-SPAWN2: NR 24 removed — **and a `git push -f` to the designated branch overwrote `b84b258`**; repaired by merging the discarded commit back | delivered (incident) | `f40ec35b`, `b84b2582` MAIN |
| 0676 → 0706 | 09-03 | spawn VM/IC/TXN; TXN and TXN2 not delivered (fallback clause); TXN3 found the claimed 14-owner blocker unreal | delivered | `c091878`, `a9e7c58`, `31c6889` MAIN; `08f99b2`, `f5c0af5` REF |
| 0710, 0714 | 09-04 | U9-FORK1 (fork had no inverse; TID leak, freed-ASID runnable TCB); U9-A64-COW2 (argument mirror destroyed `x0` on every fault) | delivered | `b545e65`, `70327f7` MAIN |
| 0718 → 0739 | 09-04 → 07 | REAP1, EXIT1–4: EXIT1 **overclaimed** NR 16 retirement (Msg 0726); EXIT2's `Complete(Err)` resumed a task with `current = None` (Msg 0730) | delivered | `f12c7c2`, `3bb83ac`, `b2850bf`, `c698c95`, `34e4748` MAIN |
| 0748 | 09-07 | EXIT2/EXIT4 relied on a Stage-189B note false since 189C6 | in progress | — |
| 0760 → 0795 | 09-07/08 | DISPATCH-CPU1–3; **CPU1 reintroduced a broad acquisition**: qualification ran only `cargo test --lib` and the "2/0/2" figure came from a hand-run grep, never the guard's scanner (Msg 0778) | delivered | `26c1c44`, `8265461`, `a556bbd` MAIN |
| 0799 → 0834 | 09-08 → 14 | TIMER1, VM-ENTRY1(-R/-L/-S), XFER1/2, IPC-RESIDUAL1–3, SEND-FINAL; `main` lagged the designated branch for a while (Msg 0799) | delivered | `ca65dca`, `cdfe859`, `7e732c0`, `19e6987`, `b997de4`, `7b58ba73`, `872c4b9`, `cdd53d4e`, `31527329`, `8dd067f4` MAIN |
| 0840 → 0870 | 09-14 → 16 | RECV-FINAL/QUEUE1/BLOCK1, FUTEX-WAIT-FINAL; the probe lane's first cut returned `WouldBlock` where canonical returns an empty result (caught by differentials) | delivered | `f24ebab2`, `8f30f3b9`, `140df976`, `bced70eb`, `6a11b948` MAIN |
| 0874 → 0914 | 09-16 → 23 | TIMER2–5, PAGEFAULT1–3 ("`Complete(Ok(()))` is not success", Msg 0907), IRQ-UNKNOWN1 (notification wake proves `{tid, asid, generation}`), IRQ-FINAL | delivered | `52e1288c`, `df93c336`, `fd23312d`, `16298722`, `f6544a58`, `fedd4ce0`, `56efc368`, `fc582912`, `ade97a65`, `ece260d0` MAIN |
| 0918 → 0928 | 09-23/24 | D6 final; U9-TERMINAL-FINAL | delivered | `e44c9b7b`, `0dce4a7f`, `7e9b5274` MAIN |
| 0932, 0936 | 09-24 | settlement and acceptance; **the previous report had called a failed RISC-V foundation-oracle profile a pass** (Msg 0932) | delivered | `f403a8d6`, `93900ae0` MAIN |
| 0942 | 09-24 | `main` fast-forwarded once 8f30f3b9 → 93900ae0 (it had not moved since 09-14) | delivered | `93900ae0` MAIN |
| 0949 | 09-24 | POST-U9-STABILIZATION: 2 fixed, 1 stale test, **x86 ordinary-cap cell still red (init at `MAX_MAPPINGS` 128/128 → `Vm(Full)`)**; host-dependent tick-count check; AArch64/RISC-V server-death runners **never live** (no initramfs supplied) | delivered, failures retained | `14b61286` MAIN |
| 0959 → 1026 | 09-25 → 30 | QEMU-BASELINE1, IRQ1/2/3, CONTEXT1, SMP1/2/3 (+ACCEPTANCE, +SEAL) | delivered | `1cfeb889`, `bfa7d33d`, `92159c05`, `f74d9295`, `569eba67`, `5c95d6be`, `e14c914a`, `dee93b13`, `f782227a`, `80085b6f`, `8ed21c1c`, `efa71dba`, `cb47d085` MAIN |

### October – November

| Msg | Date | Stage | Outcome | Commit(s) |
|---|---|---|---|---|
| 1032 → 1059 | 10-05 → 09 | QEMU-LOCK1 (+ACC, +SEAL), LOCK2, LOCK3 | delivered | `07c1fcd7`, `a828a41a`, `0fa9ec96`, `2d01c243`, `bdd0028e` MAIN |
| 1064 → 1067 | 10-09 → 12 | backlog compiled by ChatGPT from the chat history (Msg 1064, "not a fresh source audit"), worked one issue per delivery | delivered | `a91632ba` MAIN |
| 1075 → 1089 | 10-12/13 | BL1-a offset (first freeze failed on warnings), LIFETIME1 (`map_refcount` matched only an object's first frame, Msg 1078), LIFETIME2 | delivered | `96114d69`, `89273030`, `0d25d699` MAIN |
| 1096 | 10-13 | BROAD-LOCK-RETIREMENT1 (owner-proposed, Msg 1073) | delivered | `a75b1601` MAIN |
| 1102 → 1132 | 10-13 → 21 | VM-INVALIDATION1, AP-SCHED1–4 | delivered (two VMI1 live cells red, diagnosed) | `a72c69a7`, `e9c8173c`, `7a76f789`, `8cb30dd1`, `df5ee7b6` MAIN |
| 1138/1139 | 10-21 | VM-INVALIDATION2 response **cut off**; nothing changed | interrupted | — |
| 1141 → 1154 | 10-21 | VM-INVALIDATION2 (+ACC, +SEAL) | delivered | `da591ee8`, `9e4d3c49`, `63537414` MAIN |
| 1167 → 1193 | 10-26/27 | SCHED-CONTRACT1, -CORRECTION, SCHED-OVERFLOW1, KERNEL-NOFP1 (+ACC), BUILD-GATES1 | delivered | `e41b3be9`, `04b94fc0`, `91030225`, `fec063af`, `9b6c889b`, `adf5fa63` MAIN |
| 1197 → 1214 | 10-28 → 11-02 | SCHED-LOCK1 (+ACC, +SEAL; SC-LOCK-2 PARTIAL), SMP3-EVIDENCE1 | delivered | `b7f33257`, `035d0f56`, `0e21b2a6`, `298c591b` MAIN |
| 1225 → 1240 | 11-03/04 | KTHREAD1 (run 1 of 3 failed: witness kept CPU 1 busy), -ACCEPTANCE, KTHREAD-CLOSURE1 | delivered | `40132e1a`, `a7da04f1`, `136e8ae2`, `a540aef2`, `fc429b00`, `b81e0317` MAIN |

## 6. Owner proposals and how they were accepted

| Msg | Proposal | Response (ChatGPT) | What shipped |
|---|---|---|---|
| 0952 | QEMU-first IRQ/IPI tests before hardware, with recipes | adopted, with corrections: QEMU virt UART0 is PLIC IRQ **10**, not 1; `CNTP_*` = physical timer INTID 30, `CNTV_*` = virtual INTID 27; CLINT MSIP is machine-level (use the SBI IPI); QMP `qom-set` is not IRQ injection; interrupting a lock owner on its own CPU tests **self-deadlock** (Msg 0955) | QEMU-IRQ1/2/3, SMP1/2/3, LOCK1/2/3 |
| 1073 | delete the broad lock entirely | yes, after the offset fix; deleting APIs exposes callers but does not prove raw-pointer synchronization (Msg 1074) | BLR1 `a75b1601` |
| 1157 | hybrid time-partitioned + fixed-priority preemptive scheduling | adopt for a real-time profile; lock-free ≠ bounded; a bitmap bounds selection, not dispatch; `trailing_zeros()` is not one instruction on every target (RISC-V `ctz` needs Zbb); **priority cannot override temporal isolation**; IPC handoff ≠ budget donation (Msg 1160) | SCHED-CONTRACT1 (spec/model only) |
| 1164 | integer-only kernel; static per-task `USES_FP` flag; eager saving | integer-only kernel yes, but "zero FP instructions" is too strong (context routines need them); a creation flag cannot enforce non-use; LazyFP disclosure risk (Msg 1166) | KERNEL-NOFP1 now; per-task policy deferred to **FP-POLICY1** |
| 1171 | remove the shared scheduler domain lock? | yes, as SCHED-LOCK1, scheduled before preemptible kernel threads; not mandated lock-free (Msg 1173) | SCHED-LOCK1 |
| 1218 | SRP, continuations, deferred preemption, wait-free IPC | SRP is **Baker's**, admission by preemption-level/ceiling, not automatic boosting, and SMP-limited; conventional switches do not copy stacks; atomics do not make IPC wait-free; keep the audited per-CPU locks; stackful threads first (Msg 1221) | KTHREAD1 |
| 1236 | close all open items in one package | yes, staged A–D (Msg 1237) | KTHREAD-CLOSURE1 |

Other owner questions answered from source at the time (now partly superseded): userspace IRQ
handling exists as a design but was not production-usable on 09-03, and `bind_irq_notification`
checks only the Notification's SIGNAL right with no authority over the IRQ line (Msg 0688; still
true at `b81e0317`, but reachable only from the default-off UART witness [VC
`src/kernel/boot/ipc_state.rs:7465`, `mod.rs:8750`]); "per-CPU scheduler?" — per-CPU queues
under one rank-1 lock and wake-only APs on 09-03 (Msg 0693; superseded by AP-SCHED1–4 and
SCHED-LOCK1); GPIO line sharing should be a userspace lease broker above the RP1 server, whose
dispatcher then ignored sender identity (Msg 0738); `MAX_CPUS` is 64 on all three ISAs with
separate boot limits (Msg 1117; [VC] `src/arch/*/platform_layout.rs`, and production ceilings of 8
in `x86_cpu_admission.rs:36`, `aarch64_cpu_admission.rs:45`, `riscv_hart_admission.rs:49`); a
performance comparison with big-lock microkernels was answered as an expectation, never measured
(Msg 0364).

## 7. Alternate (edited) branches of the conversation

28 readable messages sit outside the export's active path. They are **not canonical**; where
Git corroborates them it is stated.

* **Alt 01–13 (08-19):** a parallel 199E delivery pass (local `main` `4ab5a8a` ABSENT; preserved
  WIP `882234e` REF on `wip/199e-riscv-default-timer`). Alt 05 noticed that `U8_RECV_TIMEOUT_*`
  markers shipped inside 199E while documents said U8 had not begun; Alt 08 re-read the directive
  file: U8 is "the single AArch64 split-return broad-lock site". Alt 10 reconciled at `e019c8a`:
  U1 49 → 47, U2 47 → 44, U3 44 → 13 with drains remaining.
* **Alt 14 (08-27):** an earlier wording of the WA3C2 re-authorization (same substance as Msg 0551).
* **Alt 15–24 (09-02):** spawn-family planning; the owner asked to remove dead NR 24
  (`SpawnProcessFromUserBuf`) and NR 26 (`SpawnFromInitramfsFile`) and to fix the shallow AArch64
  service-chain boot (Alt 18, 20). SP-1/SP-2 commits `c750af5`, `3a0df6a` are MAIN; NR 26 removal
  (`63b17065`) and NR 24 removal (`f40ec35b`) appear on the active path too (Msgs 0666–0673).
* **Alt 25–28 (09-08):** an alternative U9-TIMER1 report and review (`ca65dca`, `8c6466c` MAIN);
  notes that three raw wrapper lock calls remained while the acquisition census read 2/0/2.
