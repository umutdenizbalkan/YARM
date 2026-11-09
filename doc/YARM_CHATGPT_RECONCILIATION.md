<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Reconciliation of the owner's ChatGPT conversation with the recovered history

**Date:** 2026-11-09. **Base:** the five recovery documents preserved at `cce15d92`
(`wip/yarm-context-recovery-2026-11-05`, parent `b81e0317`). **Input:** the owner's export of
the ChatGPT conversation "YARM kernel unlock" (`6a7a20ea-c638-83eb-8679-0715c202753b`, 1 240
readable messages, 2026-08-10 → 11-04, plus 28 alternate-branch messages) and the owner's
comparison note `YARM_RECONCILIATION_FINDINGS_2026-11-09.md`. **Scope:** documentation only — no
source, test, build, branch or ref change; nothing committed or pushed; RT-SCHED1 not started.

## 1. Outcome

No end-state contradiction: both histories put the last checkpoint at `b81e0317`, the broad lock
deleted, per-CPU scheduler locks, direct NR 6/7 production-default on three ports and
cooperative kernel threads without a production user. What the five documents lacked was the
**decision history of August–September**: why the U0–U9 ladder existed and changed, which
proposals were rejected, and the large number of stopped, reverted, unpushed and rebuilt
checkpoints behind each delivered commit. Two factual errors in the 2026-11-05 documents were
found and corrected (§3).

## 2. What was recovered that the previous five documents missed

| # | Recovered information | Where it now lives |
|---|---|---|
| 1 | **The U0–U9 ladder as the owner defined it** (Msg 0012): definitions, required census movement, prompt template, `BLOCKS: U<n>` rule, ≤ 2 consecutive delta-zero passes, "markers prove execution, not retirement", WA3C2 frozen; census re-based to 49 (39 + 10; 44/3/2) from the historical 51/46 | ledger §1; unlock history §5 |
| 2 | **U0, U5 and U8**, absent from the old docs: U0 `03f6e5bd`; U5 delivered no code (exit already met); U8 complete on 08-10 | ledger §1; unlock history §5 |
| 3 | **Why priorities changed**: anti-drift reset (Msg 0008), batching for cost (Msgs 0046–0396), census ≠ effort and the WA3C2 re-authorization (Msgs 0551–0557), drift checks and the 09-08 population re-baseline (Msgs 0636, 0784–0792), unlock-then-RPi5-and-futexes (Msg 0396), QEMU-first before hardware (Msg 0952) | ledger §1; development history Eras 5–6 |
| 4 | **WA3C2 proposed vs delivered**: the first strict-publication attempt reverted for a relay hang; ChatGPT's re-proposal (Msg 0007) not accepted; directive to keep last-receiver-wins (Msg 0554); delivered semantics verified in commit and source | ledger §4; unlock history §5a |
| 5 | **A fabricated test count** ("3525 tests completed, 31 failures") reported and retracted before 08-10 | bugs B13; seed rules |
| 6 | **199C's four status changes** and **199E's four-day closure** through AArch64/RISC-V timer, GIC and FP-policy repairs, with every local-only checkpoint identified as ABSENT from `origin` | ledger §2–§3 |
| 7 | **Lost, reconstructed and incident events**: four container losses, the `f0f9890` rebuild rule, a force push and its repair, a seal truncated by the agent's own timeout, guards reverted by a base checkout, a stale-log comparison, a false WA3C2 blocker, self-caused regressions and miscounts | ledger §5; bugs C13–C16, B13–B14 |
| 8 | **21 August–September defects** (A29–A49) — waiter arbitration, scanner cursor, ownership leaks, COM1 breadcrumbs, GIC DTB order and per-root mapping, `sscratch` drift, user FP by accident, ambient `current_cpu`, register save, `x15` zeroing, MemoryObject reclaim, fork/teardown inverses, exit settlement, a NR 7 regression, an accounting false failure, the mapping-capacity cell, the IRQ-binding authority gap | bugs ledger Part A |
| 9 | **Owner proposals and their corrections** (QEMU recipes, RT hybrid scheduling, no-FP kernel and FP flag, scheduler-lock removal, SRP/continuations, closure bundling) and the BL backlog's origin as a chat-history list | ledger §6; development history Eras 6–8 |
| 10 | **Workflow facts**: auto fast-forward after green WIP (from Msg 0521), `main` lagging the designated branch 09-14 → 09-24, branch alignment on 08-11 | unlock history §5 |
| 11 | **Estimates vs reality** (9–14, 6–10, 8–12 passes; U9 closed 09-24) | ledger §1; bugs B16 |
| 12 | **Alternate branches**: a parallel 199E pass, the U8 marker-name collision, the owner's NR 24/26 removal requests, an alternate TIMER1 review — with Git corroboration | ledger §7 |
| 13 | **Owner-deferred work**: futex requeue/bitset/PI, AP breadcrumb removal, GPIO lease broker, production userspace IRQ path | seed §7 items 12–14 |

## 3. Errors in the 2026-11-05 documents, corrected

1. **WA3C2 chronology.** `YARM_KERNEL_UNLOCK_HISTORY.md` §4 placed WA3C2 and the restored x86
   default in 07-29 → 08-10. Correct: frozen 08-10 (Msg 0012), delivered 08-31 (`612339b5`)
   [VH]. Fixed in §4, §5 and the development history.
2. **x86 CPU ceiling.** The documents said no x86 ceiling constant existed and labelled ≤ 8 as
   SR. Source: `PRODUCTION_CPU_CEILING = 8` in `src/arch/x86_cpu_admission.rs:36` (also RISC-V
   `riscv_hart_admission.rs:49`, AArch64 `aarch64_cpu_admission.rs:45`); `MAX_CPUS = 64` [VC].
   Fixed in the seed, unlock history and development history.
3. **Precision, not error:** the U7 row "production IPC-timeout pipeline on all three arches"
   (`8275c927`) is the commit's own subject, but default reachability on AArch64/RISC-V and the
   199E closure came two days later (`e019c8aa`); the row now says so.
4. **Dated statements.** "No newer work on any branch" and "the ChatGPT conversation is
   entirely unavailable" were true on 2026-11-05; they are now dated rather than deleted.

## 4. Contradictions explained (none silently resolved)

| Topic | Statements | Resolution |
|---|---|---|
| WA3C2 semantics | "adopt strict single-waiter publication" (Msg 0007) vs. delivered last-receiver-wins | different things: a rejected proposal and the shipped design [VH, VC] |
| U8 | "COMPLETE" (Msg 0012) vs. "U8 is next" (Msgs 0190–0318) | ChatGPT error, corrected at Msgs 0319/0320; an alternate branch found a marker-name collision (`U8_RECV_TIMEOUT_*` inside 199E) |
| 199C | complete (0188) → open (0319) → deferred (0416) → closed (0418) | only the last stands (`b59b7225`) |
| WA3C2 SMP gate | "no direct call/reply has ever crossed CPUs" (0558) vs. existing cross-CPU profiles (0560) | wrong profile used; the gate was met with the existing profiles (0561) |
| ChatGPT-derived seed | U8 work on 08-19; 199C complete at 0188; U3 "toward 13"; x86 ceiling open | inaccuracies in an owner-side summary; Git/source answer them (sources §6.14) |
| Owner findings note | asks to verify the delivered WA3C2 policy | verified: §2 item 4 |

## 5. Verification performed

* **Commit provenance.** Every hexadecimal commit cited in `YARM_STAGE_LEDGER.md` was resolved
  against a fresh full clone of `origin` (3 186 commits on `main`, 147 branches): **174 on
  `main`, 17 only on `wip/*` refs, 26 not present at all** — and every one matches the label
  given in the ledger. In the conversation as a whole, 620 hexadecimal tokens were checked: 377
  commits on `main`, 82 on other refs, 79 tree objects, 82 not objects (unpushed or amended
  checkpoints, lost work, artifact checksums).
* **Source checks at `b81e0317`:** WA3C2 displacement and refusal (`ipc_state.rs:1971-2000`,
  `waiter_ownership.rs:971-1021`, tests `a_displacement_retires_the_departing_and_arms_the_arriving`);
  direct-IPC default on three arches (`mod.rs:4154`, introduced by `2aab8b77`); notification
  waiters as exact records (`defs.rs:453`, `signal_notification`); `bind_irq_notification`
  authority and callers (`ipc_state.rs:7465`, `mod.rs:8750`); CPU ceilings and `MAX_CPUS`.
* **Not performed:** no build, test or QEMU run; reports quoted from the conversation remain
  [SR] unless a commit or source line is named.

## 6. Unresolved

1. Whether a displaced untimed receiver can remain `Blocked` (no test; [INF]).
2. The text of the owner's 08-10 directive and review files (not in the export).
3. Whether later packages fixed the carried POST-U9 failures (x86 ordinary-cap cell at
   `MAX_MAPPINGS`; AArch64/RISC-V server-death runners without initramfs).
4. The RPi5 hardware outcome after BOOT-4 (unchanged from 2026-11-05).
5. The full 310-row mechanical ledger: generated, then destroyed with the export by a container
   restart on 2026-11-09; regenerate from the export if needed.
6. Older stale-document items from 2026-11-05 remain (STATUS.md, AI_AGENT_RULES.md §3.4/§6,
   KERNEL_UNLOCKING.md §0.3 wording, comment near `mod.rs:4024`).

## 7. Files changed (working tree, uncommitted)

Five documents updated in place relative to `cce15d92` — `YARM_MASTER_CONTEXT_SEED.md` (reviewed
consolidated seed), `YARM_DEVELOPMENT_HISTORY.md`, `YARM_KERNEL_UNLOCK_HISTORY.md`,
`YARM_BUGS_AND_LESSONS.md`, `YARM_RECOVERY_SOURCES.md` — and two new files:
`YARM_STAGE_LEDGER.md` and this report. The name avoids the `_REPORT.md` suffix that
`tests/doc_fragmentation_guard.rs` forbids in `doc/`.
