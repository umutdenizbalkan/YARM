# RISCV-ECALL-A7-RET1 — predeclared qualification schedule

* **Candidate F** = `3091f143`, the frozen head of `wip/riscv-ecall-a7-ret1` (worktree `ea7-wt`,
  clean, pushed): witness `1a570728`, repair `53b96510`, contract tests `9b84e5c1`, raw-UART
  `1bcfc294` + `3091f143`, c01 order `42263677`, docs `a6cb2307`.
* **Baseline B** = `3ac4f578` (RISCV-PARK-RET1, the canonical dev head), worktree `rpr-wt`, clean.
* **B+W** = `1a570728`: B plus only the default-off witness (no production change) — the live
  baseline reproducer.
* **Runner:** `ea7_chain.sh`, in order: (1) hosted baseline reproduction (`ea7_baseline.sh`: F's
  `ecall_contract` module inserted into B, nothing else); (2) the hosted negative controls at F
  (`ea7_nc_run.sh` + `ea7_nc.py`, the FULL hosted lib per control, 11 controls and an unmodified
  run); (3) live (`ea7_live.sh`); (4) the matrix (`ea7_matrix.sh`). Sequential; every step runs
  whatever earlier steps returned; no retries in place; a rerun is appended, never substituted, and
  declared here first; logs, rcs, durations, HEAD, porcelain and every `ARTIFACT_BINDING` kept under
  `ea7-evidence`.

## Expected

* **(1)** On B: 15 of the 23 `ecall_contract` tests fail — c01, c03, c03b, c04, c05, c06, c07, c09,
  c10, c11, c12, c13, c16, c17, c21 — each on `x13` (`a3`) or `x17` (`a7`); c00, c02, c08, c14, c15,
  c18, c19_c20, c22 pass.
* **(2)** Each control fails at least one permanent test BY BEHAVIOUR (a compile failure does not
  count); the unmodified tree passes all:

| id | reintroduced defect | expected to fail (at least) |
|---|---|---|
| NC1 | the switched ordinary arm's `a7 = 0` (bridge and idle) | c05, c09, c17, c21 |
| NC2 | the wrapper declares `a2` an INPUT (`in("a2")`) — an incorrect operand declaration | c01 |
| NC3 | `a7` overwritten after the continuation arm restored it | c04, c10, c11 |
| NC4 | a syscall result applied to an async-preempted task (async arm `a0 = ret0`) | c14 |
| NC5 | startup arguments over a live continuation (continuation arm `a0 = arg(0)`) | c04, c10 |
| NC6 | a completion of another blocked generation accepted | t11 and the existing exactly-once tests |
| NC7 | the parked FutexWait `a0 = address` defect (FutexWait left out of the park commit) | t02, c05 |
| NC8 | the parked Yield argument-return defect (Yield left out of the park commit) | t09, c08 |
| NC9 | `sepc` advanced twice (the park commit adds 4) | t20, c05 |
| NC10 | a delivered completion consumed again (the take leaves it parked) | the exactly-once tests |
| NC11 | the same-task arm's `a3 = 0` / error | c03, c06, c07 |

* **(3) live, RISC-V SMP 2, the ecall witness (`scripts/qemu-riscv-ecall-witness-smoke.sh`):**
  * **LB ×2** at B+W: must FAIL, and only on `a3` / `a7` (`lost_a3 > 0`, `lost_a7 > 0`,
    `lost_other = 0`, every coverage class present).
  * **LF ×3** at F: `result=ok`, `lost_* = 0`, every coverage class present (switched and same-task
    Yield, parked / woken-before-park / mismatch FutexWait, requeue then wake, blocked receive,
    timer-preempted spin, fresh entries).
  * **L-NC1** (F + NC1) and **L-NC11** (F + NC11): must fail the witness, on `a7` and on `a3`
    respectively.
  * **LW**: the FUTEX-WAIT-ATOMIC1 wait witness at F, ×1 per ISA (`parked_err = 0`; a run that fails
    ONLY on a damaged marker is reported as such and counted as a failed run).
  * Acceptance: all LF pass, all LB / L-NC fail as stated, every LW passes or fails only on an
    attributed damaged marker on a comparison ISA.

## The matrix (4)

| id | step | expected |
|---|---|---|
| P0 / P1 | provenance; source shape (diff stat vs B; the arms' A3/A7 lines; no `A7] = 0` / `A3] = err` left) | HEAD=F, porcelain 0 |
| H1 / H2 / H2b | fmt; clippy hosted; classes and counts vs B | identical classes and counts |
| H3 / H4 | hosted lib; integration (incl. `broad_lock_census_guard`, `doc_fragmentation_guard`, `x86_64_ap_env_scope`) | 0 failed |
| H5 | subsets: ecall_contract, riscv_park_ret1, riscv64_async_preemption, stage184, stage199d, futex_requeue1::wait_atomic1, futex_requeue1::atomic1, futex_requeue1, futex_wake_cap1, futex_found1, futex, trapframe, async, riscv, u6, blocked, recv, send, scheduler, sched_, ap_sched1, syscall, kthread, exit, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common tests | no change except the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race` |
| G1 / G2 | contract gate (ABI v11) on F / on B | rc 0 / rc 0 |
| G3 / G4 | marker-scan self-test; wait, requeue and ecall grader self-tests | ok |
| D1–D3, O1–O3 | default builds ×3; IPC recv-v2 oracles ×3 | rc 0 |
| UN | §8 negative control on F's clean default artifacts: the unmodified core smoke replays its own recorded passing boot (pass) and the same boot with one REQUIRED marker byte-split (fail, naming it), x86_64 and AArch64 | as stated |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal | 12/12 live cells (idle-oracle variance tracked separately) |
| S7 | CI strict timer grep | inherited debt #6 |
| W | IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| FX2–FX6 | overtaken ×2; AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| FX7 / FX8 | FUTEX-REQUEUE1 ×3; FUTEX-REQUEUE-ATOMIC1 race ×3 | `result=ok cross_cpu=yes`; `covered=3` |
| R1–R3 | RISC-V D2 settle (F, B); NR 1 source-fault send ×3 (F; RISC-V also B); queued cap-bearing reply ×3 (F; RISC-V also B) | rc 0; F as B |
| FB0–FB3 | B: strict builds, the seal, the overtaken witnesses | as on F |
| U1–U3, M1–M4 | IRQ/UART/PL011 witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles (incl. smp2-core, smp4-core), reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| AP | secondary-CPU startup and marker integrity: every F log with x86_64 AP bring-up (SMP ≥ 2) scanned for byte-split `X86_AP_*` markers and Rust-entry ladder bytes; every F AArch64 log for raw `3ASW3` / `4ASW4`; the same counts on B's logs | F: 0 split, 0 ladder bytes, 0 raw; reported for B |
| X | section sizes and sha256 of the three default kernels, F vs B (fail-closed extraction) | reported: RISC-V, x86_64 and AArch64 `.text` all differ (each port changed) |
| FP | footprint: `.text` / `.rodata` / `.data` / `.bss` sizes per ISA F vs B; the RISC-V write-back's instruction counts | reported |
| Z | every QEMU log of F through `fwa_zscan.sh` | reported per file; any hit explained |

The six baseline debts, d2-send and the idle-oracle variance stay separately classified; nothing new
is classified as inherited without a reproduction on B.

## Addendum A (declared 2026-11-12 20:05Z, after reading the live phase, before any rerun)

Live phase as run (kept, not repaired): LB1, LB2 FAIL on `a3` / `a7` only (as required); LF3
PASSES; **LF1 and LF2 FAIL** — so the predeclared "all LF pass" is NOT met — with `lost_a3 = lost_a7 =
lost_other = 0` in both: LF1 on coverage (no FutexWait woken before it physically parked: 0 of 24
waits that run; LF2 5, LF3 7), LF2 on the witness's own "solo" rule (Y0's FIRST solo Yield, cycle 25,
was resumed by a switch: the drain dequeued Y0 from the last finishing role's own FutexWait park —
`RISCV_STARTUP_ARGS tid=10008 entering=10013` — the role that had just incremented DONE was still
running; cycles 26..32 were same-task). L-NC1, L-NC11 fail as required; LW ×3 pass.

Both failures are defects of the default-off WITNESS, not of the contract it measures. Declared,
appended:
* **W'** — a witness-only commit on top of F (no production line): the "solo" phase stays as a
  coverage aid but its per-cycle rule is withdrawn (the same-task Yield coverage requirement stays);
  the waiter runs 48 cycles instead of 24 (so a run is far less likely to see no early wake); the
  grader counts solo cycles resumed by a switch, informationally.
* **F'** = F + W'. **XW**: the three default kernels and initramfs images at F and F' — section
  sha256 identical (W' touches only `riscv-ecall-witness`-gated code and scripts); if identical, every
  default-build matrix result at F stands for F'.
* **LF' ×3** at F' (acceptance: all pass, `lost_* = 0`, full coverage), **L-NC1' and L-NC11'** at
  F' + NC (must fail on `a7` / `a3`), **LB'** ×1 at B+W+W' (must fail on `a3` / `a7` only), hosted lib
  + integration + grader self-test at F'.
* The verdict records LF1 and LF2 as failed runs of the original witness, with this cause.

## Addendum B (declared 2026-11-12 22:10Z, after a container restart, before any rerun)

The session container restarted at about 22:05–22:07Z while the matrix was in
`C2_ci_profiles_F` (started 22:04:49). Every process died; no rc was recorded for C2. All steps
before it (P0 … M4, C1) had completed and their rcs stand. This is an infrastructure interruption,
not a result: the partial C2 log and logs dir are kept, renamed `*.interrupted*`, unrepaired.
Declared, appended: the matrix is resumed with `ONLY="ci xsect"` (C1 rebuilt, then C2 … C5d, AP,
X/FP, Z), its lines appended to the same `summary.txt` under a `matrix start … only=ci xsect`
header; Addendum A then runs as declared. Nothing before C2 is rerun or substituted. If the
resumed C2 differs from the C4 baseline in a way the inherited classification does not explain,
that is a finding, not a reason to rerun.

## Addendum C (declared 2026-11-12 23:35Z, after reading the matrix tail and Addendum A, before any rerun)

Read, kept as recorded:
* `XW kernels_identical=NO` — the only DIFF lines are `.yarm_nofp_map` on x86_64 and AArch64, with
  F=`EMPTY` and F'=`e3b0c44298fc1c14` (the SHA-256 of zero bytes). The XW loop labels a zero-byte
  extraction `EMPTY` on the F side only, so an empty section on both sides compares unequal.
* `AP_marker_integrity rc=1` — every F-side hit is in a file that is not an F boot: the
  `oracle-C4-*` directories (copied after C4, which runs on B; their `ARTIFACT_BINDING` carries
  B's x86_64 kernel `672f9fb7…`, not F's `0f9a107f…`) and `UN/x86_damaged.log` (the negative
  control's deliberately split replay). The scan's path filter misses both.
Declared, appended (the original lines and rcs stay): **XW2** — rebuild the three default kernels
and initramfs at F and F' and compare every non-debug PROGBITS/NOTE section with one symmetric
rule (size and sha256 of the extraction, the same label on both sides), and compare the section
header tables; **AP2** — the same AP scan with B-produced `oracle-C4-*` and the `UN/` control
replays classified on the B / control side, every reclassified file listed with its binding.
Neither changes a grader or any pass/fail rule in the repository.
