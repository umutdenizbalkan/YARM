#### IRQ-DELIVERY1 (from delivered `c703226`)

**Findings.** All were confirmed on `c703226` with Q1–Q7, which use real binds, deliveries, the
shared trap dispatch and the bridge.

| Test | Result on `c703226` | Defect or behaviour |
|---|---|---|
| Q1 | fails | One interrupt past a full queue returned `EndpointQueueFull` and its line was lost |
| Q2, Q3 | fail | The shared trap dispatch returned `Err(Syscall(EndpointQueueFull))` for an interrupt taken from user and from idle context |
| Q4 | fails | The bridge settled the saturated delivery as `Some(Err)` |
| Q6 | fails | With two lines saturated, the overflowing line was never observable |
| Q5 | passes | Ordinary delivery was correct |
| Q7 | passes | Destruction already retired the queued events |

Live, the default-off saturation witness was applied to `c703226` without the repair. On every
port it delivered 16 interrupts, then one `outcome=failed`, then a fatal halt:

* AArch64: `YARM_AARCH64_TRAP_HANDLE halting`;
* RISC-V: `RISCV_TRAP_HANDLE_FAILED`;
* x86_64: the fatal breadcrumb for vector 0x24, then the host timeout.

The interrupt had been acknowledged first: there was no storm and no corrupted frame. The logs
are kept as `live-baseline`; the first attempt is kept as `live-baseline-attempt1` (see the
RISC-V fixture note below).

**Repair.** The contract is in `doc/IPC.md` §8.11.

* *The state.* A Notification keeps its bounded queue, plus `overflow_pending`, one bit per IRQ
  line.
* *Signal.* A full queue sets the line's bit, and repeats coalesce. While any bit is set the
  object stays in coalescing mode, so no line starves.
* *Receive.* A receive takes the queue, then the lowest pending line, and clears exactly that
  bit. The message ABI is unchanged.
* *Outcomes.* The split delivery reports `Coalesced` (marker `coalesced`, no error) and the broad
  form returns `Ok`. `Failed` now covers invariant failures only.
* *Not changed.* No other error class changed meaning, and trap errors are not suppressed. The
  one completion owner still runs exactly once for every ending.
* *Not added:* no syscall, capability type, rights bit, lock or unbounded storage.

Commits:

* code `1611de3`;
* regressions `ba0296a`;
* the default-off saturation witness `edf4ac9`, `60abd7f` and `3b7aa56`.

**Live saturation witness.** This is the `irq-saturation-witness` feature, mutually exclusive
with the kernel-thread witness.

* After the eight ordinary items, init parks with its depth-8 queue empty. The host then sends
  ten device bytes, each paced on the previous interrupt's completion:
  * x86_64 and AArch64 take them from the idle hart;
  * RISC-V, whose fixture gates at user return, takes them on user returns.
* Eight interrupts queue and two coalesce. Init then drains nine events (the eight queued lines
  plus the pending line) and the UART data.
* The grader (`scripts/grade-irq-saturation-witness.py`) requires, from the kernel's own
  markers:
  * 16 `delivered`, 2 `coalesced`, 9 received;
  * claims equal to completions (18 = 18);
  * no fatal marker.

**Qualification of `3b7aa56`.** This followed a predeclared schedule. The steps ran
sequentially and none was retried. Logs, rcs, artifact bindings and hashes are kept.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `c703226` |
| hosted lib (serial); integration (`--no-fail-fast`) | 6375 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: irq_delivery1, u9irq1, u9irqfinal, irq_auth1, irq, notification, notif_waiter1, notif_lifetime1, irq_generation1, stage21, stage22, stage23, ipc_displace1, selected_arch_trap_entry, u9f_split, revoke, reap | 15, 33, 15, 15, 123, 50, 19, 16, 15, 11, 10, 6, 19, 5, 33, 78, 47 passed |
| lock order, common test set | per test and pair; see the attribution below |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| UART/PL011 IRQ witness ×3 (unchanged contract) | 8/8 deliveries each, `delivered` only |
| IRQ saturation witness x86_64 / AArch64 / RISC-V | `IRQSAT_GRADE result=ok` on each: 16 delivered, 2 coalesced, 9 received, 18 claims = 18 completions; saturation from idle on x86_64 and AArch64, from user on RISC-V |
| kernel-thread IRQ witness ×3 | `KIRQ_VERDICT failures=0` |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on clean default bindings, F (kernel `310b0144`) and B (`ae876b68`) | smp2-core and smp4-core pass on both. sender-wake ×2 and sched-timeout fail with the inherited signatures, identically on both |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |
| every QEMU log of F: `outcome=failed`, the three ports' fatal channels, `KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED`, route release/retraction/reclaim, `NOTIF_REF_OVER_RELEASE` | none |

*Lock-order attribution.* The tracker is hosted and sequential.

* The predeclared changes, all of them seen:
  * the three reworked queue-full tests (renamed, so they appear under their new names on F).
    They now drain with real receives and no longer take the error path;
  * the race test `c09_reply_vs_timeout_terminal_cas_race` (+2 and +10), which varies
    between runs of the unchanged baseline.
* One change outside the predeclaration: `long_run_multi_core_simulation_is_deterministic`,
  `ipc←capability` −107 and `task←ipc` −107.
  * Cause: that test calls `ipc_recv` only when `handle_trap_event` returns an error. On
    `c703226` the full queue made that happen 107 times; with the repair it never does.
  * Isolated reruns, three on each side, give identical counts every time (F 70/227, B
    177/334). This is appended to the evidence; it is not a retry of the step.
* Nothing in production code took a new acquisition, and the closure guard still proves nothing
  is nested.

**Hosted negative controls.** Each removes one protection, in a disposable worktree, and all 10
were detected. The unmodified candidate passes all 196 tests in the subset. On the baseline tree
(`c703226` plus only the completion counter and Q1–Q7), Q1–Q4 and Q6 fail.

| Control | Protection removed | Detected by |
|---|---|---|
| NC1 | saturation is fatal again (`EndpointQueueFull`) | 16 tests: Q1–Q4, Q6, I2–I8, exact completion, starvation, both producers, the object owner, the three reworked tests |
| NC2 | silent loss (coalescing sets nothing) | 14 tests, including Q1–Q3, Q6 and I2–I8 |
| NC3 | a pending line is never reported | 13 tests, including Q1–Q3, Q6, I2–I4 and I8 |
| NC4 | a receive clears every pending bit | Q6, I2/I3, starvation, object owner, broad route |
| NC5 | the split delivery's generation check removed | I5–I7, B10 |
| NC6 | a missing completion | exact completion, Q2–Q5, decline/claim test |
| NC7 | a duplicate completion | exact completion and 13 more |
| NC8 | a saturated user interrupt writes the frame | Q2 |
| NC9 | `Coalesced` propagates an error | 10 tests: Q1–Q4, Q6, exact completion, the outcome-family test, the three reworked tests |
| NC10 | no coalescing mode (a busy line refills the queue) | starvation, object owner |

**Limits.**

* A coalesced line means "at least one interrupt". A driver that needs a count reads its
  device.
* A level-triggered source that stays asserted re-raises after every completion. Masking or
  device acknowledgement is driver work, recorded as an open limitation, not in scope.
* SMP correctness is shown at the rank-3 linearization point with hosted, serial schedules (I2–I8
  enter the split delivery for a second CPU inside the window). It is not driven concurrently on
  two online CPUs.
* The live saturation origins are idle on x86_64 and AArch64 and user on RISC-V. Kernel-thread
  origin is covered by the unchanged kernel-thread witness and hosted tests, not by saturation.
* The RISC-V UART fixture's idle-only gate stalled a long saturation sequence on both trees.
  The saturation items now open at user return; the ordinary items are unchanged. The flaky
  runs are kept (`id1-try/riscv64-run2`, `run3`, `live-baseline-attempt1`).
* No default image binds an IRQ line, so saturation is not reachable in production today.
* The six baseline debts, the NOTIF-WAITER1 sender-waiter observation and the IRQ-AUTH1
  endpoint-removal observation remain as recorded.

Verdict: **IRQ_DELIVERY1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-delivery1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.
