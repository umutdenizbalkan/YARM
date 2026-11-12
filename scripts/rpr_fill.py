import sys
p = sys.argv[1]; t = open(p).read()
R = {}
R["@@BASE@@"] = """8 of its 24 tests fail — `t02`, `t05`, `t06`, `t07`, `t08`, `t09`,
  `t19` and the order guard `g05` — every behavioural one on the same mechanism: ring 3 resumes a
  genuinely parked `FutexWait` with `a0` = `0x60000` (the address), `a1` = `5` (the expected value),
  and a `Yield` with its own arguments `(7, 8, 9)`; the other 16 (own-frame classes, send and receive
  completions, the async arm, a fresh task, the exit and stale-incarnation refusals, `sepc`, `a3..a5`)
  pass on `2a91a6e8` as on the candidate (`baseline_repro_2a91a6e8.log`)."""
R["@@NC@@"] = """**Tests and controls.** `futex_requeue1::riscv_park_ret1`: 19 behavioural tests (`t01`–`t12`,
`t15`, `t17`–`t22`) drive the production bridge arms, park, wake (same CPU, other CPU, requeue),
capture, restore and every RISC-V write-back arm, and read the ring-3 registers; five guards
(`g01`–`g05`) pin the write-back arms, the user-runtime decode and `ecall` operand list, the route's
`Parked` result and the bridge order. 24 pass on the candidate. Ten negative controls, each applied
to the candidate and run against the FULL hosted lib (6537 tests); each fails by behaviour, none by
compilation (`build_errors=1` in each is only cargo's "test failed" footer), and the unmodified tree
passes all:

| control | mutation | caught by (failed tests) |
|---|---|---|
| NC1 | the resumed `a0` keeps the syscall's argument | 7 (`t02 t05 t06 t07 t08 t09 t19`) |
| NC2 | the final result is never committed | 6 |
| NC3 | committed into the register file only (the switched arm's argument install overwrites it) | 7 |
| NC4 | published for an exiting incarnation | 1 (`t15`) |
| NC5 | a send completion of an older cycle accepted | 4 (`t11` and three existing U6 / overtaken tests) |
| NC6 | a syscall result applied to an asynchronously preempted task | 5 (`t18` and four async-preemption tests) |
| NC7 | the commit overwrites the whole argument mirror `a0..a5` | 3 |
| NC8 | `sepc` advanced a second time | 5 (incl. `t20`) |
| NC9 | a delivered completion consumed again | 11 (`t11` and ten existing exactly-once tests) |
| NC10 | STRUCTURAL: the settlement takes a global lock | 1 (`t22`) — production has no broad lock to reintroduce (`SharedKernel::with` is test-only); any other lock the commit took is found by `t22`, a `with`/`with_cpu` callsite by `broad_lock_census_guard` |"""
R["@@LIVE@@"] = """**Live** (SMP 2, the revised wait witness, frozen candidate `194842bc`; schedule predeclared):

| run | result | parked | parked_ok | parked_err | mismatch | during publication | dup/missing |
|---|---|---|---|---|---|---|---|
| RISC-V 1 / 2 / 3 | `witness=ok core_smoke=ok result=ok` ×3 | 18 / 24 / 23 | 18 / 24 / 23 | 0 | 5 / 3 / 3 | 9 / 5 / 6 | 0 |
| AArch64 1 | FAIL: damaged marker `USER_LOG3 …FWAW_CALL cycle=10` (lost call line → cycle 10 missing, two waits for cycle 11) | 6 | 6 | 0 | 13 | 12 | 1 |
| AArch64 2 | FAIL: damaged marker `FUTEX_WAIT_WITNESS_3SKIPPED` | 4 | 4 | 0 | 14 | 14 | 0 |
| AArch64 3 | `result=ok` | 4 | 4 | 0 | 13 | 15 | 0 |
| x86_64 1 / 2 / 3 | `result=ok` ×3 | 7 / 9 / 11 | 7 / 9 / 11 | 0 | 14 / 15 / 10 | 11 / 8 / 11 | 0 |
| L-NC2 RISC-V (no commit) | FAIL, as it must: `parked_err=24`, "a GENUINELY parked wait … returned Err(Internal)" | 24 | 0 | 24 | 3 | 5 | 0 |
| L-NC2 x86_64 (same patch) | `result=ok`, as it must (the commit is RISC-V only) | 8 | 8 | 0 | 14 | 10 | 0 |

Both AArch64 failures are the damaged-marker case the schedule names: each damaged byte sits between
the other CPU's `ASW2` and `ASW3` lines, i.e. `activate_asid`'s `raw_uart_marker(b'3')`, written
outside the printk drain lock (`KERNEL_UNLOCK_DEFERRED_DEFECTS.md`); no `parked_err` in either; the
logs are kept unrepaired and counted as failed runs, not rerun. The predeclared acceptance — all
three RISC-V runs pass, at least one clean run per comparison ISA, no `parked_err` in any run —
holds. Before the repair (`c2b9893e` plus only the witness) the same RISC-V boundary returned `Err`
16 of 16."""
R["@@WITNESS@@"] = """The witness now opens a window only while a sibling of the waiter is current on
another CPU (peer gate, from FUTEX-WAIT-ATOMIC1's undelivered `ae65c57e`, cherry-picked as
`c30b0eba`), with two signallers, so a waiter can no longer spin, interrupts masked, while its only
waker is queued behind it on the same hart. A held window ends `woke`, `no_peer` (an explicit
refusal: the peer stayed absent for 8 rechecks × 64K polls) or `bound` (50M polls, a failure); no
unbounded masked spin is left. 32 cycles. The grader fails closed on a missing, corrupted or
duplicate OPEN, a CLOSE or in-window wake without OPEN, a wrong window identity, an incomplete final
window and a damaged witness marker (narrowed so `FORK_PROOF_COW_WITNESS_SKIPPED` is not one); it
correlates every userspace `FWAW_CALL` with the kernel results since the previous call and reports
`mismatch`, `during_publication`, `parked`, `parked_ok`, `parked_err`, `dup_or_missing`, requiring
`parked_ok >= 2` and `parked_err = 0`. Its self-test covers each refusal with a fixture. Logs are
never repaired."""
R["@@FOOTPRINT@@"] = """RISC-V default kernel `.text`: `0xb03ee` → `0xb05ee` (+512 bytes);
`riscv_commit_final_park_result` is 288 bytes and the outgoing capture is now an out-of-line
function (278 bytes; inlined on `2a91a6e8`). x86_64 and AArch64 compile to byte-identical `.text`
(below)."""
R["@@QUAL@@"] = """**Qualification** (frozen candidate `194842bc`, clean tree, against baseline `2a91a6e8`, clean
tree; schedule written before the run and kept with its sha256, three addenda each declared after
reading a result and before running anything for it; sequential, no retries in place; every log, rc,
duration, HEAD, porcelain and `ARTIFACT_BINDING` kept):

| cell | result |
|---|---|
| provenance; source shape | HEAD `194842bc`, porcelain 0; `src/arch/x86_64`, `src/arch/aarch64`, `src/arch/trap_entry.rs` unchanged; `riscv_commit_final_park_result` called once in production, by the RISC-V bridge, before the capture |
| fmt; clippy (hosted) | rc 0; 425 warnings in the same classes and counts as `2a91a6e8` |
| hosted lib, serial | 6537 passed, 0 failed (3 ignored) |
| integration | 41 binaries, 396 passed, 0 failed (incl. `broad_lock_census_guard`) |
| 34 subsets (`riscv_park_ret1` … `sender_wake`) | 0 failed each |
| lock order per (test, pair), against `2a91a6e8`, 6513 common tests | only `c09_reply_vs_timeout_terminal_cas_race` (the known run-to-run variant); `FUTEX_WAKE_STOPPED` in the same tests on both |
| contract gate | rc 0 on both |
| marker-scan self-test; grader self-tests | all cases hold; ok |
| default builds ×3; recv-v2 oracles ×3 | rc 0 |
| CI-exact strict builds ×3; strict core smokes ×3 | rc 0 |
| first-cohort futex seal, candidate and baseline | 12/12 live cells and every idle cross-check present, on both (the idle-oracle variance did not recur) |
| CI strict timer grep | 0/0/0, inherited #6 |
| IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| overtaken ×2 (candidate and baseline); AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| FUTEX-REQUEUE1 live witness ×3 | AArch64, RISC-V `result=ok cross_cpu=yes`. x86_64 FAILED: `witness=ok cross_cpu=yes`, but the core smoke rejected the boot on `X86_AP_KERNEL_CR3_BEGIN`, logged as `X86_AP_KEiRNEL_CRy3_BEGIN cupu=1` — the AP trampoline's unlocked raw bytes `i`, `y`, `u` (`smp_trampoline.rs`) inside the BSP's marker (see below) |
| FUTEX-REQUEUE-ATOMIC1 race witness ×3 | `covered=3` each |
| RISC-V D2 empty-current settle; NR 1 source-fault send ×3; queued cap-bearing reply ×3 | rc 0; the RISC-V results identical on `2a91a6e8` |
| baseline: strict builds, seal, overtaken ×2 | as on the candidate |
| IRQ witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| CI profiles, candidate and baseline | identical: sender-wake, smp2-core, smp4-core pass; inherited #2, #3 (`D6_SMP_DISPATCH_OK`), #4 (no idle-entry timeout) and d2-send fail on both |
| reply-timeout matrix; SMP-request script | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67), identical on both |
| `.text` identity (fail-closed, Addendum C) | identical at the candidate and the baseline for the AArch64 default kernel, the x86_64 default kernel and the x86_64 futex-requeue-witness kernel, every extraction size-checked against its header; `.rodata` differs only in five panic-location line words (deltas +51 and +26, the lines inserted in `boot/mod.rs` and `runtime.rs`) |
| marker scan (the matrix's candidate logs) | one hit: the FX7 x86_64 seal line above; none in the appended reruns or the live logs |

*The x86_64 FUTEX-REQUEUE1 failure* (Addendum A). Its byte-split marker has the mechanism of the
baseline's own logs — `X86_AP_ENTER_RUrST`, `X86_AP_PcERCPU_BEGIN`, `X86_AP_nPiyuERCPU_RECORD_READY`
in `2a91a6e8`'s seal and overtaken boots, which passed only because those markers are not required —
and of an earlier stage's record (`X86_AP_TKlERNEL_CR3_BEGIN`, same marker). Appended and declared:
the same command twice more on the candidate and twice on the baseline, all four `witness=ok
core_smoke=ok result=ok cross_cpu=yes`; and that kernel's `.text` is byte-identical at the two
commits (Addendum C). It is classified as the x86 SMP serial interleaving, separate from the six
debts. Two addenda corrected the identity check itself: the first `.text` comparison's AArch64 half
(and the first witness-kernel comparison) had hashed EMPTY extractions — `objcopy … /dev/stdout`
into a pipe yields nothing here — which Addendum B's fail-closed check then refused (rc 1); Addendum
C extracts to files and checks sizes. Both void logs are kept."""
R["@@LIMITS@@"] = """
* *Committed classes are FutexWait and Yield only:* both are final when the caller leaves the CPU.
  A blocking receive and send keep their completion machinery; a class added later that parks
  with a final result must be added to `riscv_commit_final_park_result` (`AI_AGENT_RULES.md`).
* *`a7` after an untagged switched resume is `0`* (pre-existing; no RISC-V user binary reads it
  after an `ecall` on any straight-line path — 554 `ecall`s in 14 binaries scanned), recorded, not
  changed.
* *The live witness proves FutexWait* on all three ISAs; Yield's committed `0`, requeue, exit and
  stale-incarnation refusals are proven hosted.
* *Console damage, not repaired here:* AArch64 `raw_uart_marker` bytes from `activate_asid`; x86_64
  AP trampoline raw bytes. Both cost failed runs in this qualification and are recorded.
* *Still open:* the six baseline debts and d2-send, unchanged; the idle-oracle variance (not seen
  this run)."""
R["@@VERDICT@@"] = "RISCV_PARK_RET1_ACCEPTED_WITH_BASELINE_DEBT"
for k, v in R.items():
    assert t.count(k) == 1, k
    t = t.replace(k, v)
assert "@@" not in t
open(p, "w").write(t)
print("filled")
