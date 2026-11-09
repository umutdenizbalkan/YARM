<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM bugs and lessons

Labels: **[VC]** verified in source at `b81e0317`; **[VH]** verified from commits/committed
records; **[SR]** session-reported (compaction summaries of `session_01QLpmtT4bLN2gX3DCKAXYp8`,
Oct–Nov 2026); **[INF]** inferred. Sources: `doc/YARM_RECOVERY_SOURCES.md`.

Part A is a ledger of real defects and their root causes. Part B is the set of failure
patterns that AI agents have repeatedly fallen into on this project, with concrete examples and
the rule that prevents each. Part C is operational hazards of the environment.

---

## Part A — Bug ledger (selected, highest regression risk first)

| # | Defect | Root cause | Fix / evidence | Regression risk |
|---|---|---|---|---|
| A1 | RISC-V device interrupts on kernel threads silently broken (KTHREAD-CLOSURE1 Stage C run 1) | the new kernel-thread root lacked root slot 255 — the device window through which the kernel reaches device pages under any translation except Bare; `mmio_va_under_active_satp` walks the live root | `bb451a3b`: `enter_root` installs the window's root entry; control `rv_no_device_window` fails the witness [VH] | **high**: any new page-table root must carry every mapping the kernel relies on under that root |
| A2 | AArch64 overflow probe looped forever (Stage C dev) | a pre-indexed store fault does not move `sp`; deep in the gap bit 14 of `sp` is clear, so the bit-14 entry test missed it | the synchronous entry always moves to a per-CPU emergency stack before any store [VH] | high if someone "simplifies" the vector |
| A3 | `IPC_SERVER_DEATH_LINK_LEAK created=54 detached=1 result=fail` | not a leak: one counter pair answered two questions | 199D scoped the accounting to one ServerDies transaction; leak invariant moved to system-wide totals [VH, `doc/IPC.md` §8.5] | medium: counters must have one meaning |
| A4 | `destroy_endpoint` leaked the direct-ack lease and stranded the parked receiver | raw `endpoint_waiters[idx] = None` bypassed the accessor family | WA3C1 `10b91677`: central `remove_endpoint_waiter_at`; wake only if `{tid, asid, wait_generation, Blocked}` still match [VH, PR #542] | high: never clear owned state directly |
| A5 | displaced waiter's ack lease never released | displacement path skipped release | WA3C1 [VH] | |
| A6 | spawn endpoint leaked on `CapabilityFull` | endpoint installed under rank 3, rank released, caps minted under rank 4 — order kept, atomicity lost; ledger recorded an index, not an identity | U9-SPAWN-IC1 `31c68896`: hold rank 3 then 4 (`with_ipc_then_capability_mut`), restore slot AND generation on failure, record full identity [VH] | high: "avoid lock-order inversion" must not become "give up atomicity" |
| A7 | U9-DISPATCH-CPU1 reintroduced a broad-lock acquisition | refactor without re-running the census | U9-DISPATCH-CPU2 `8265461b` removed it; census contradiction reproduced first [VH] | the census guard exists for this |
| A8 | missing `Running` mark on dispatch | owner never set it on one path | U9-DISPATCH-CPU3 `a556bbde` [VH] | |
| A9 | U9-EXIT2's post-clear settlement unsafe as delivered | proof rested on a premise that EXIT3 found false | EXIT3 `c698c950` token; EXIT4 races; RESIDUAL1 corrected EXIT4's stale admission fact [VH] | high: re-verify premises, not conclusions |
| A10 | U9-TIMER1 introduced a RISC-V resume defect | conversion changed resume ownership on one port | TIMER1 §4b repair [VH] | cross-arch |
| A11 | x86 D6 proof crash after cleanup | `fxrstor` from an all-zero area set `MXCSR = 0` (all SSE exceptions unmasked) | Stage 131 `b5874437`: initialize frame FPU state [VH] | |
| A12 | x86 D6 post-proof "#XF" | actually #PF on an unmapped kernel stack: only the top page was mapped while the trap handler descends ~9 KB | Stage 132: map all stack pages [VH] | |
| A13 | AP user-memory consumption failed | AP `EFER.NXE` not set | 199A2D2C2B3 `ccceb03d` [VH] | per-CPU setup must match the BSP |
| A14 | ~209 KB stack frame in `ensure_pt_allocator_initialized` | large local | Stage 135 `c16dca2e` [VH] | the KTHREAD-CLOSURE1 image check now bounds single decrements |
| A15 | fork COW `Vm(Full)` | address-space capacity | Stage 163D raised `MAX_ADDRESS_SPACES` 32 → 48 [VH] — **later briefs forbid capacity increases as fixes** | |
| A16 | send enqueue woke a recv-v2 waiter without delivery (receiver resumed with a0=cap); park capture overwrote the WouldBlock publication | two real kernel races found by the AP-SCHED1 witness | publish WouldBlock; reapply after capture [SR, AP-SCHED1] | |
| A17 | lost server death on RISC-V | pre-existing; reproduced on base with hosted test + forced live window | park/take/requeue (AP-SCHED3) [SR] | |
| A18 | x86 dispatch-gap invalidation hole | per-page unmap shootdown targeted placement, not root residency | VM-INVALIDATION2 `da591ee8` [VH] | |
| A19 | VMI1 settlements reported success with an unacknowledged invalidation | completion was not typed | VM-INVALIDATION1 `a72c69a7`: unacknowledged = failure; quarantine permanent [VH] | |
| A20 | user-origin x86 #GP and RISC-V illegal instruction halted the kernel | decoded to `TrapEvent::Unknown`, settled fatally once the broad per-task arm became unreachable | BL2 / BL2-a (`d38f641d`…`d43940f2`): the task's fault [VH] | closing a broad path can strand behaviour that only it had |
| A21 | FutexWait parked even when the word changed | missing re-check | BL3b: park only while the word still equals `expected` [VH] | |
| A22 | quantum and hardware deadline conflated | one constant used for both | BL4b `455c9520`: separate `BOOTSTRAP_TIMER_DEADLINE_TICKS` and `SCHED_QUANTUM_TICKS` [VH] | |
| A23 | printk dropped in-flight records; graders read truncated lines | `PRB_MSG_MAX = 192`: whole-record lines lost their checksum | BL5d hold the record; witnesses split long lines (VMI2-ACC, SMP2_SYNC_P2) [VH/SR] | every new witness line must fit 192 bytes |
| A24 | kernel FP/SIMD use (22 755 XMM instructions x86, 2 429 q/v AArch64) | compiler-generated in ordinary kernel code | KERNEL-NOFP1 `fec063af` + final-image checker [VH] | checker runs on every build path (BUILD-GATES1) |
| A25 | plain `SpinLock` held across yield/block/exit undetected | `switch_check` only saw interrupt masking | KTHREAD-CLOSURE1 Stage B `a540aef2`: per-stack plain-guard counting, refusal, latched accounting failures [VH] | |
| A26 | Stage B lock path overflowed | `then_some(v + 1)` evaluated eagerly on a poisoned `u32::MAX` | lazy `then(|| v + 1)` [SR] | |
| A27 | unused-variable warning on every build (Stage C run 1 QC6) | run-view bound used only by a default-off witness | `ca98bd61` [VH] | warnings are qualification failures |
| A28 | parallel hosted tests: 58–71 shared-state assertion failures | process-global test state; earlier real memory corruption fixed (`doc/KERNEL_TEST_RULES.md` H1) | run `--test-threads=1` [VH] | blocks using the hosted suite as a torture harness |

Grader / checker false passes (each closed by an ACCEPTANCE or SEAL package): NFA (KERNEL-NOFP1
checker prefix + exclusion), SL1A (four scheduler-lock grader false passes), SLS (execution after
removal; no-ASID bypass), S3E (residency and IPI attribution), VMI2-ACC / VMI2-SEAL (mapping-owner
decision, gap-overlap causality, observer identity), KTA §4 (lifecycle grader), KTHREAD-CLOSURE1
A1 (replay overlap, timeout sequences). [VH]

---

## Part B — Recurring failure patterns and the rules that prevent them

### B1. Reintroducing fixed bugs or acting on stale facts
*Examples:* two consumers read a stale heading ("DISABLED on every architecture — 199D-WA1-GATE")
instead of the code, and U9-RESIDUAL1 listed NR 6/7 as having no split route (U9-YIELD2 §1); a
comment in `src/kernel/boot/mod.rs` (~4024) still says AArch64/RISC-V direct IPC is proof-gated
while the function returns true [VC]; EXIT4's proof rested on a stale fact (U9-RESIDUAL1 §1);
`doc/STATUS.md` §1 still says BSP-only. [VH]
*Rule:* **code is authoritative over comments and docs.** Before relying on any statement,
re-derive it from current source and say where. When you find a stale statement, correct it in
the same change or record it.

### B2. Violating architectural invariants
*Examples:* A4 (`destroy_endpoint` bypassing the owner), A6 (atomicity traded for lock order),
index-as-identity in the spawn ledger, A1 (a new root missing a mapping every other root has).
*Rule:* every state transition goes through its **one owner**; identities carry generations;
rollback restores generation as well as slot; a new address-space root must justify every
mapping it omits. Read `doc/KERNEL_UNLOCKING.md` §3 (invariants) and §4 (fixes to preserve)
first.

### B3. Ignoring stage acceptance criteria
*Examples:* directive U3 "complete" while canonical 203C stayed open; arithmetic once reported
"1 of 34" canonical stages instead of 3 of 35 (`doc/KERNEL_UNLOCKING.md` §0.4); U6 explicitly
labelled "PARTIAL COHORT (canonical 199C stays OPEN)".
*Rule:* canonical stages are owner-defined; a directive never completes one unless the
definition's full scope holds on every required architecture. Report "not re-classified"
rather than guessing.

### B4. Confusing test-only or proof mechanisms with production behaviour
*Examples:* direct IPC was live-proven for weeks under default-off knobs before the first
production evidence (`0b5ec254`); kernel threads are created only by tests and witnesses [VC];
D6 switch-proof harness vs production D6-genuine; `d6_genuine_enabled()` is x86-only [VC].
*Rule:* every claim states which level it reached — hosted foundation / live proof (knob-gated
or not) / production default / stage complete — and on which architectures. Default-off
witnesses may **observe** production owners; they may never implement the repair.

### B5. Treating partial implementations as complete
*Examples:* U9-EXIT1's retirement claim corrected by EXIT2; EXIT2 unsafe → EXIT3; refused and
deferred seals (`STAGE_199_IPCCALL_REPLY_OFFLOCK_SEAL … result=deferred`, Stage 161 "DEFERRED with
precise blocker (not faked)"); ServerDies 1 of 3 cells; `ExitCurrentTask` RISC-V cell never earned.
*Rule:* list what is **not** done next to what is. A cell that was not earned is recorded as
not earned.

### B6. Changing code without understanding callers
*Examples:* U9-DISPATCH-CPU1 reintroduced a broad acquisition (A7); BLR1's owner warning "do not
infer safe ownership merely from zero callers of the old wrappers"; witness hooks placed on one
RISC-V trap path missed `handle_riscv_trap_entry_shared` [SR].
*Rule:* enumerate every caller and every entry path per architecture before editing; re-run
the census guard (`tests/broad_lock_census_guard.rs`) and the scope guards.

### B7. Breaking another architecture while fixing one
*Examples:* A1 (RISC-V-only root), A10 (RISC-V resume after a timer conversion), AArch64 SMP2
verifier race (closure 3), x86 baseline guards encoding wake-only assumptions (AP-SCHED4).
*Rule:* every change is qualified on **x86_64, AArch64 and RISC-V**; "compile-only" on a port is
disclosed, never presented as passing.

### B8. Masking failures by relaxing tests
*Examples (what was refused):* checkers relaxed to "marker present"; loosened totals; guards
silently updated. Owner rule (backlog brief): "derive the obligation from production events;
reject missing, duplicated, reordered or substituted evidence."
*Rule:* a guard that pins old text is **re-derived**, with the reason recorded — never weakened.
Every repair gets a negative control that fails without it. If a control unexpectedly passes,
the design claim is wrong (KTHREAD-CLOSURE1: the AArch64 bit-14 control revealed the test was
not the defence).

### B9. Losing rollback and failure-path correctness
*Examples:* U9-VM-ENTRY1-R (rollback contract incomplete), -L (backing lifetime), SRL1/SRL2
(shared-region lifetime), spawn ledger (A6), VMI1 (unacknowledged invalidation reported as
success).
*Rule:* for every transaction, write the failure table first: what each failure point leaves,
who releases it, and that a stale token can never release a replacement.

### B10. Declaring success without sufficient evidence
*Examples:* hosted tests passing while SMP behaviour was wrong (A16, A18 found only by live
witnesses); "boot success does not prove Rust aliasing safety" (BLR1 brief); runner defects that
silently skipped checks (KTHREAD-CLOSURE1 QC16/QC17, declared and recorded).
*Rule:* hosted tests never prove SMP or hardware behaviour. Qualification = predeclared schedule,
frozen commit, fresh artifacts, every result kept, failures diagnosed and re-frozen; a passing
retry never replaces a failed scheduled run.

### B11. Large speculative refactors instead of bounded increments
*Examples (correct behaviour):* HARD-STOP audits 185B/185V/186B/186D4/187D; U9-B "STOP — the
four-site cohort is two fences, not one"; one issue per delivery in the BL backlog.
*Rule:* one independently reviewable issue per delivery; if the safe seam does not exist, stop
and record why.

### B12. Shortcut fixes the owner forbids
Forbidden as fixes (repeated in briefs): capacity increases, broad-lock fallback or re-acquisition,
renamed global locks, weakened guards, oracle-only production fixes, unrelated cleanup, changing
ABIs. (A15 is the historical counter-example from before the rule.)

---

## Part C — Operational hazards in this environment [SR, repeated]

1. **`pkill -f` / `pgrep -f` patterns killed the agent's own shell** (exit 144) at least ten
   times. Kill by PID only.
2. **Branching from a stale local `main`.** Always branch from fetched `origin/*` refs or a
   verified commit; the local checkout may be shallow and old.
3. **Editing or committing tracked files while a boot builds** triggers artifact-binding
   refusals; run qualification from a detached worktree of the frozen commit.
4. **Editing a bash script while it runs** shifts bash's read offset; keep executing runners
   immutable (write a new revision file).
5. **Heavy builds during QEMU qualification** skew timing-sensitive witnesses.
6. **Disk exhaustion** (linker bus error, ENOSPC broke gates) — delete scratch `target/` dirs,
   gzip big logs; check `df` before long runs.
7. **Container restarts** lose processes (and once QEMU and the shallow clone state); resume
   from a checkpoint, record the restart.
8. **Background tool limit (~2 h)** — launch long runners detached (`setsid nohup … &`).
9. **rustfmt reflows** break text-anchored edit scripts and source guards.
10. **printk 192-byte line limit** truncates witness records.
11. **Hosted tests must run `--test-threads=1` and `CARGO_INCREMENTAL=0`**; the release
    lib-test profile is broken on some bases (use test profile with opt-level 2 for benchmarks).
12. QEMU may be absent after a restart: reinstall (`qemu-system-x86 qemu-system-arm
    qemu-system-misc opensbi u-boot-qemu`) — and disclose "QEMU not available" rather than
    claiming a pass (`doc/AI_AGENT_RULES.md` §13.6).
