# SCHED-TIME-FOUNDATION1 — predeclared qualification schedule (E)

* **Candidate F** = `c4c209a9a0992a0d4ea3869afdb23fe47147e761`, the frozen head of `wip/sched-time-foundation1` (worktree `stfq-wt`,
  clean, pushed): A `f7df3839` + `1218d51c`, B `dc61a01c`, C `1dee980a` + `ae5e538c` (witness
  hardening), D `84dfa607`, QUAL `c4c209a9` (docs, the RISC-V smoke NR 20 term, the c08 stale-token test).
* **Baseline B** = `8062c216` (FUTEX-TIME1, the accepted dev head), worktree `stfbase-wt`, clean.
  B has no NR 18/19/20 and no sleep witness; it is the comparison for every inherited suite, and
  the reproduction base for A's defects (already recorded: `stf-evidence/A/repro_*_baseline.log`).
* **Runner** `stf_chain.sh`: (1) hosted negative controls at F (`stf_nc_run.sh` + `stf_nc.py`, the
  FULL hosted lib per control, `--test-threads=1`, one unmodified run `F_control`); (2) live
  (`stf_live.sh`); (3) the matrix (`stf_matrix.sh`). Sequential; every step runs whatever earlier
  steps returned; no retries in place; a rerun is appended, never substituted, and is declared
  here first with a timestamp; logs, rcs, durations, HEAD, porcelain and every `ARTIFACT_BINDING`
  kept under `stf-evidence/qual`. No QEMU artifact step sets `CARGO_TARGET_DIR`.
* **Interrupted steps** (a container or worker restart): a step without a recorded `rc` is rerun
  once from that step, its partial log kept with suffix `.interrupted`; a step with a recorded
  result is never rerun.
* **Development runs before the freeze** (kept, not qualification): `stf-evidence/B_live` (clock
  smoke + addendum-1, classified there), `stf-evidence/C_live` and `C_live2` (sleep witness
  development), `stf-evidence/A/nc` and `stf-evidence/BC/nc` (checkpoint controls; ncC8 did not apply
  there and ncC5 exposed the c08 gap — both are re-run here at F).
* **Hosted NC set at F:** ncA1–ncA10, ncB1–ncB7, ncC1–ncC10, F_control (28 runs).

## Expected

### (1) Hosted negative controls — each must fail >= 1 permanent test BY BEHAVIOUR

| id | defect | expected to fail (at least) |
|---|---|---|
| ncA1 | rotation refusal restored | r01–r12, sfr1_k01/k02 |
| ncA2 | duplicate current on rotation | r11 (or hang; rc 124 with failures counts) |
| ncA3 | rotation at the wrong priority (High whatever the class) | many behavioural failures (and a hang: rc 124 with failures counts) |
| ncA4 | dispatch rollback lost | r01, r11–r13, sfr1_k01 |
| ncA5 | reservations ignored by the rotation | r07 |
| ncA6 | receive timeout Runnable before placement | i01, i07 |
| ncA7 | send timeout refusal discarded | i02 |
| ncA8 | timeout and reply both complete | i05 |
| ncA9 | stale receive generation accepted | u8_production_timeout_coverage::a_stale_receive_generation_cannot_disturb_a_replacement |
| ncA10 | deferred server death dropped | i04 |
| ncB1 | wrong frequency | b02 |
| ncB2 | unsynchronized source not clamped | b04 |
| ncB3 | conversion overflow wraps | b03 |
| ncB4 | uninitialized realtime exposed | b09, s02 |
| ncB5 | realtime reader ignores a write in progress | b10 |
| ncB6 | torn user copy | s03 |
| ncB7 | realtime authorization dropped | s04 |
| ncC1 | sleep due 1 µs early | sleep1 c04 |
| ncC2 | sleep completion published as FutexWait TimedOut | c04 |
| ncC3 | refused placement makes the sleeper Runnable | c09 |
| ncC4 | exit does not retire the sleep record | c07 |
| ncC5 | stale token accepted by the timeout claim | c08 |
| ncC6 | a claimed sleep record survives (double completion) | c06/c08 |
| ncC7 | deadline overflow wraps | c12/c13 |
| ncC8 | realtime sleep before an epoch | c02/c13 |
| ncC9 | a reached deadline still parks | c03/c14 |
| ncC10 | resume boundary forgets the Sleep class | c17 |

A surviving control is reported with its test gap and not counted. `F_control` must pass
everything.

### (2) Live, SMP 2

* **LST-<isa>-<n>**, n = 1..3, ISA = riscv64, aarch64, x86_64 — the sched-time witness:
  `SCHED_TIME_WITNESS_SEAL result=ok` (rules: `scripts/grade-sched-time-witness.py`; the twelve
  sleep cases, the error lanes, realtime, full-queue rotation, the IPC timeout deferral).
* **L-ncL1** (riscv64, sleeps due 50 ms early) and **L-ncL2** (x86_64, a Sleep completion not
  committed to the restored registers): must FAIL the grader.
* **LT-<isa>** — the FUTEX-TIME1 witness x1 per ISA (regression; FT1's grader unchanged).
* **LW-<isa>** — the FUTEX-WAIT-ATOMIC1 wait witness; **LE** — the RISC-V ecall witness
  (RISCV-ECALL-A7-RET1); **LS-<isa>** — the IRQ saturation witness. (Built/failed exactly as on
  B is classified as baseline; FT1 recorded LW/LE build failures on its candidate.)

### (3) The matrix — FUTEX-TIME1's (`ft1_matrix.sh`), with F and B substituted

P0/P1 provenance; H1 fmt; H2/H2b clippy and classes vs B; H3 hosted lib; H4 integration; H5 subsets
(adds `sleep1`, `sched_full_rot1`, `sched_time_foundation1`, `timekeeping`, `stf1_`); H6 lock order
vs B; G1/G2 contract gate F and B; G3/G4 marker-scan and grader self-tests (adds the sched-time
grader); D1–D3 default builds; O1–O3 recv-v2 oracles; UN damaged-marker control; S1–S7 strict builds
and core smokes; FX1 first-cohort seal; W IPC-DISPLACE1; FX2–FX6 overtaken, AP scheduling, KTHREAD1,
SCHED-LOCK1; FX7/FX8 FUTEX-REQUEUE1; R1–R3 RISC-V D2 settle / NR 1 fault / queued cap; FB0–FB3
baseline strict / seal / overtaken; U1–U3 UART/PL011 IRQ; M1–M4 direct matrix, server-dies; C1–C5 CI
profiles, reply-timeout matrix, SMP request on F and B; AP marker integrity; X/FP sections and
footprint F vs B (E4: the printk ring's +~170 KB `.bss` is expected); Z marker scan.

Expected: 0 failed anywhere F differs from B; FUTEX-TIME1's recorded baseline debts stay classified
and identical on F and B; nothing new is classified as debt without the same failure on B.

### Addendum A — declared 2026-11-14T10:36:38Z (during the live phase, before LE/LS and the matrix)

Trigger: LW-riscv64 at F sealed no coverage (windows=0, every wait `FUTEX_WAIT_WITNESS_SKIPPED
reason=no_peer_running`); waiter and both signallers ran on cpu 1 (FT1's LW-riscv64-C: one signaller
on cpu 0, 13 windows). `src/kernel/task_enqueue.rs` (balanced placement) is unchanged 8062c216..F.
Question: placement variance, or a systematic change? Runs (`stf_addA.sh`), after `stf_chain.done`,
sequential, interleaved, each in its own fresh detached worktree, nothing substituted (LW-riscv64 stays
recorded as failed): LWA-B-1, LWA-F-1, LWA-B-2, LWA-F-2, LWA-B-3, LWA-F-3 (B = 8062c216). Recorded
per run: seal, and the cpu of tids 10008/10009/10010. Reading: if B and F alike show both outcomes (or
F seals as often as B), LW-riscv64 is classified as placement variance; if F never covers while B
does, it is a regression and is investigated before any verdict.

### Addendum B — declared 2026-11-14T10:47:01Z (live phase; before LS-aarch64/riscv64 and the matrix)

Trigger: LE at F failed coverage only ("no FutexWait woken before it physically parked": wait_early=0,
wait_parked=36) with lost_a3=lost_a7=lost_other=0 (the RISCV-ECALL-A7-RET1 contract held); FT1's
LE-C: wait_early=14, wait_parked=14. Runs (`stf_addB.sh`), after Addendum A, same discipline:
LEB-B-1, LEB-F-1, LEB-B-2, LEB-F-2, LEB-B-3, LEB-F-3 (B = 8062c216), fresh detached worktree each;
LE stays recorded as failed. Reading: as Addendum A (variance if B shows the same spread; a
regression if F never covers while B does).
