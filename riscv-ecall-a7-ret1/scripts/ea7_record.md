
#### RISCV-ECALL-A7-RET1 (from delivered `3ac4f578`)

**The question.** Every RISC-V userspace `ecall` (`yarm-user-rt`'s `raw_syscall`) declares `a0..a2`
`inlateout` and `a3`, `a4`, `a5`, `a7` `in` — the compiler may keep a live value in an input
register across the call. Does every kernel resume convention honour that, and in particular the
switched ordinary write-back, which wrote `a7 = 0`?

**The defect, reproduced** (before any production change).

* *Hosted, deterministic* (`futex_requeue1::riscv_park_ret1::ecall_contract`, 23 tests that
  evaluate the write-back arms' own `frame.regs[RiscvTrapFrame::X] = …;` statements parsed from
  `boot.rs`, including the error/success branches and the mirror loop's skip set, and panic on an
  expression or condition they do not model): run on `3ac4f578` with nothing else changed, 15 of 23
  fail — `c01`, `c03`, `c03b`, `c04`…`c07`, `c09`…`c13`, `c16`, `c17`, `c21` — each on `x13` (`a3`)
  or `x17` (`a7`), exactly as predicted; the other 8 pass on both trees.
* *Live, ring 3* (`riscv-ecall-witness`, default off; `3ac4f578` plus only the witness, `1a570728`,
  RISC-V SMP 2): every probe is an `asm!` `ecall` with every register `inlateout` and a key in
  `a6`; the grader compares each register after the call with what went in. Two scheduled runs:
  `lost_a3=30 lost_a7=28 lost_other=0` and `lost_a3=10 lost_a7=34 lost_other=0`.
* *Two arms, not one.* The switched ordinary arm (bridge and S-mode idle) forced `a7 = 0`; the
  same-task arm and both continuation arms wrote `a3` = the error code or `0` — the old error lane
  — although the ABI returns the error in `a0` and `a3` is an input.

**Resume conventions** (RISC-V, at the candidate; `a0..a2` are results, everything else is restored):

| convention | arm | `a0..a2` | `a3` | `a4`, `a5` | `a7` | `sepc` |
|---|---|---|---|---|---|---|
| same-task return | same-task | result (error in `a0`) | saved (`user_gprs[13]`) | saved | saved | +4, once |
| switched, untagged (Yield `a0=0`, parked FutexWait `a0=1`, fresh startup) | switched ordinary, bridge and idle | committed result or startup args | args lane | args lane | saved (`user_gprs[17]`; 0 for a fresh task) | committed |
| blocking IPC, reply / timeout, completion before or after the capture | continuation, bridge and idle | completion result | saved | saved | saved | +4, once |
| timer preemption | async, bridge and idle | verbatim register file | verbatim | verbatim | verbatim | interrupted pc |
| exit / stale incarnation / stale dispatch token | none | — | — | — | — | — |

**The repair (Option A: the kernel preserves).** The saved register file is already the source of
truth and costs one load per register: every arm now writes `a3 = user_gpr(13)` and both switched
ordinary arms `a7 = user_gpr(17)`. No publisher writes `user_gprs[13]`, `user_gprs[17]` or
`args[3..5]`; spawn, exec and TCB creation build zeroed GPRs, so a fresh task still starts with
`a7 = 0`; a fork child copies its parent's context and now sees the fork's number in `a7`, as the
parent does. Option B (declare `a3`, `a7` clobbered in every wrapper) was rejected: it changes the
user ABI, costs every caller, and leaves the kernel arms disagreeing with each other. Six existing
guards that pinned `A7 = 0` or `A3 = err` were re-derived from the contract, not deleted.
Dispatch tokens, generations, async and continuation tags are untouched; no bare-TID authority,
no new table, field, static or allocation.

**Raw console bytes (§8).** *AArch64:* `activate_asid`'s two `raw_uart_marker` writes (outside the
printk drain lock) are removed. *x86_64:* the AP's Rust-entry breadcrumb ladder (`yarm_x86_64_ap_entry`
naked asm, from `@` onward) now goes to debugcon `0xE9` instead of COM1; the pre-Rust trampoline
ladder (`g` … `>`) stays on COM1 — it has no stage word or lock to use, and no logging, allocation
or lock was added to it — as do the fatal-trap `debug_uart_putc` breadcrumbs. The negative control
replays a recorded passing boot through the unmodified core smoke (pass) and the same boot with one
required marker byte-split (fail, naming it), on x86_64 and AArch64; no grader was relaxed.

**Tests.** 23 `ecall_contract` tests (`c00` checks the extraction itself), plus the re-derived guards
(`g01`, the idle three-conventions guard, the verbatim-restore guard, the stage-184 lanes, the
stage-199D same-task lanes, `riscv_round_trip…`) and `x86_64_ap_env_scope`'s two re-derived
breadcrumb guards. Hosted lib 6560 passed, 0 failed.

**Negative controls** (each reintroduces one defect into the candidate; full hosted lib per control;
0 compile errors in all 11; the unmodified tree passes all):

| id | defect | failed by behaviour |
|---|---|---|
| NC1 | switched arm `a7 = 0` (bridge and idle) | 11, incl. `c05`, `c09`, `c17`, `c21`, `g01` |
| NC2 | wrapper declares `a2` an input | `c01`, `g03` |
| NC3 | `a7` overwritten after the continuation arm | `c01`, `c04`, `c10`, `c11` + 1 |
| NC4 | syscall result applied to an async-preempted task | `c14`, `g01` + 1 |
| NC5 | startup arguments over a live continuation | `c04`, `c10`, `g01` + 1 |
| NC6 | completion of another blocked generation accepted | `t11` + 3 exactly-once tests |
| NC7 | parked FutexWait `a0 = address` | 8, incl. `t02`, `c05` |
| NC8 | parked Yield returns its arguments | `t09` (predicted also `c08`: it does not fail — that Yield's `a0` input is `0`, equal to its result) |
| NC9 | `sepc` advanced twice | 11, incl. `t20`, `c05`, `c19_c20` |
| NC10 | delivered completion consumed again | 11 exactly-once tests |
| NC11 | same-task arm `a3 = 0` | `c01`, `c03`, `c03b`, `c06`, `c07`, stage-199D |

Live: `L-NC1` fails on `lost_a7=25`, `L-NC11` on `lost_a3=11`, `lost_other=0` in both.

**Live qualification** (RISC-V SMP 2; coverage per run: fresh entries, switched and same-task Yield,
parked / own-frame / woken-before-park / mismatching FutexWait, requeue then target wake, blocked
receive, timer-preempted spin):

| run | tree | result |
|---|---|---|
| LB1, LB2 | `1a570728` (baseline + witness) | FAIL, as required: `lost_a3=30/10`, `lost_a7=28/34`, `lost_other=0` |
| LF1 | `3091f143` | **FAIL** — coverage: no FutexWait woken before it parked (0 of 24); `lost_*=0` |
| LF2 | `3091f143` | **FAIL** — the witness's own "solo" rule: Y0's first solo Yield was resumed by a switch (the drain dequeued it from the last finishing role's own FutexWait park); `lost_*=0` |
| LF3 | `3091f143` | `result=ok`, `lost_*=0` |
| L-NC1, L-NC11 | `3091f143` + NC | FAIL, as required (above) |
| LW ×3 ISAs | `3091f143` | FUTEX-WAIT-ATOMIC1 witness `result=ok`, `parked_err=0` |
| LF′1–3 | `83be1921` | `result=ok` ×3, `lost_*=0`, full coverage (`wait_early` 14 / 17 / 10, `yield_same` 9 / 8 / 9) |
| L-NC1′, L-NC11′ | `83be1921` + NC | FAIL, as required: `lost_a7=37`; `lost_a3=45`; `lost_other=0` |
| LB′ | `1a570728` + W′ | FAIL, as required: `lost_a3=21 lost_a7=52 lost_other=0` (this run saw no early wake) |

LF1 and LF2 are recorded as failed runs of the original witness, with no register lost in either.
Addendum A (declared after reading them, before any rerun) added the witness-only commit W′
(`83be1921`: the solo phase stays as coverage, its per-cycle rule is withdrawn and counted
informationally as `solo_switched`; the waiter runs 48 cycles instead of 24). The default kernels
and initramfs images are byte-identical at `3091f143` and `83be1921` on all three ISAs (every
section by size and sha256, and the section header tables), so every default-build result below
holds for `83be1921`.

**Qualification** (frozen candidate `3091f143`, clean tree, against baseline `3ac4f578`, clean tree;
schedule written before the run and kept with its sha256, three addenda each declared after reading
a result and before running anything for it; sequential, no retries in place; every log, rc,
duration, HEAD, porcelain and `ARTIFACT_BINDING` kept). A container restart killed the run during
the candidate's CI profiles (no rc recorded); Addendum B resumed it from that group, the interrupted
log kept.

| cell | result |
|---|---|
| provenance; source shape | HEAD `3091f143`, porcelain 0; no `A7] = 0` or `A3] = err` left in `boot.rs` |
| fmt; clippy (hosted) | rc 0; 424 warnings in the same classes and counts as `3ac4f578` |
| hosted lib, serial; integration | 6560 passed, 0 failed (3 ignored); 41 binaries, 396 passed, 0 failed (incl. `broad_lock_census_guard`, `doc_fragmentation_guard`, `x86_64_ap_env_scope`) |
| 30 subsets (`ecall_contract` … `sender_wake`) | 0 failed each |
| lock order per (test, pair), 21507 common entries | only `c09_reply_vs_timeout_terminal_cas_race` (the known run-to-run variant); `FUTEX_WAKE_STOPPED` in the same tests on both |
| contract gate (ABI v11) | rc 0 on both |
| marker-scan, wait / requeue / ecall grader self-tests | all cases hold |
| default builds ×3; recv-v2 oracles ×3; strict builds and strict core smokes ×3 | rc 0 |
| §8 negative control | verbatim replay passes, damaged replay fails naming `X86_AP_KERNEL_CR3_BEGIN` / the AArch64 split-dispatch markers |
| first-cohort futex seal, candidate and baseline | 12/12 live cells on both (the idle-oracle variance did not recur) |
| CI strict timer grep | 0/0/0, inherited #6 |
| IPC-DISPLACE1 SMP1 ×3; overtaken ×2 (both trees); AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| FUTEX-REQUEUE1 ×3; FUTEX-REQUEUE-ATOMIC1 race ×3 | `result=ok cross_cpu=yes` ×3 (x86_64 included); `covered=3` ×3 |
| RISC-V D2 settle; NR 1 source-fault send ×3; queued cap-bearing reply ×3 | rc 0; RISC-V identical on `3ac4f578` |
| IRQ / UART / PL011 witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| CI profiles | candidate: sender-wake, smp2-core, smp4-core pass. Baseline: the same except **smp2-core FAILS**, `X86_AP_SCHED_ADMIT_BEGIN` missing — logged `iX86_AP_SCHEyD_AuDMIT_BEGIN cpu=1`, the Rust-entry ladder's `y`, `u` on COM1, the §8 defect this stage repairs. Both: inherited #2, #3 (`D6_SMP_DISPATCH_OK`), #4 (no idle-entry timeout) and d2-send |
| reply-timeout matrix; SMP-request script | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67), identical on both |
| AP marker integrity | candidate: 32 x86_64 AP logs, 26 AArch64 logs — 0 split markers, 0 ladder bytes on COM1, 0 raw TTBR0 bytes. Baseline: 15 of 16 x86_64 AP logs split (48), 7 of 7 AArch64 logs with raw bytes (2020) |
| marker scan | 248 candidate logs and every addendum log, 0 hits |

Two scan errors were corrected by appended checks (Addendum C) without changing a grader: the XW
loop labelled an empty section `EMPTY` on one side only (`.yarm_nofp_map`, 0 bytes on both), and
the AP scan counted the baseline's saved `oracle-C4-*` oracle directories (B's kernel hash) and the
negative control's deliberately split replay as candidate logs. Both original lines and rcs are kept.

**Locks and footprint.** The repair takes no lock: it reads two fields of the context the arm
already holds. Lock order unchanged (above), `broad_lock_census_guard` unchanged. RISC-V: the only
text symbol that changed size is `yarm_riscv64_trap_bridge`, 4016 → 4014 bytes, 1245 → 1245
instructions (stores 322 → 321, loads 124 → 127, `sd zero` 6 → 4), branches 57 = 57, no atomics
added, stack frame `0x380` unchanged; interrupt-disabled path length unchanged; `.text` −2 bytes,
`.rodata`, `.data`, `.bss` unchanged. x86_64: every section the same size (`mov dx, 0x3F8` → `0xE9`
has the same encoding length; the ladder writes the same number of bytes, to another port).
AArch64: `.text` +152 (`activate_asid` −48 for the removed markers; the rest a `core::fmt` helper
reshuffle — `DebugList::entries<&u8>` +540, `DebugInner::entry` −312, `[u8; 6]::fmt` −132),
`.rodata` −32, `.bss` −4096 (one page of section alignment). Early startup: two fewer raw UART
writes per AArch64 ASID activation; no instruction added to any pre-Rust trampoline.

**Limits.**
* *The contract is the wrapper's:* `a0..a2` results, everything else preserved. A future wrapper
  that declares more outputs is safe; one that declares fewer must not ship without a kernel arm
  that preserves them (`AI_AGENT_RULES.md`, `SYSCALL_ABI.md`).
* *Supersedes* RISCV-PARK-RET1's recorded limit "`a7` after an untagged switched resume is `0`".
* *Open observation:* a remotely completed receive leaves `a2` holding its length argument; `a2` is
  an output of every wrapper, so no caller reads it, but the value is not a result
  (`KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).
* *The live witness is RISC-V only*; AArch64 declares `x0..x5` clobbered and x86_64 uses its own
  convention — unchanged.
* *The x86_64 pre-Rust trampoline ladder* still shares COM1 (no safe bounded fix without a lock or
  stage word); no qualified log of the candidate showed a split from it.
* *Still open:* the six baseline debts and d2-send, unchanged; the idle-oracle variance (not seen
  this run).

Verdict: **RISCV_ECALL_A7_RET1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/riscv-ecall-a7-ret1`
at the commit that adds this record; `main` `b81e0317` and every other ref unchanged.

