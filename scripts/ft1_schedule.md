# FUTEX-TIME1 — predeclared qualification schedule

* **Candidate F** = `04a09b2b`, the frozen head of `wip/futex-time1` (worktree `ft1-wt`, clean,
  pushed): kernel core `73ba1971`, user-rt + 41 tests `ef7e8a3f`, witness kernel half `2f3be94d`,
  witness cell/smoke/grader `a4be8174` (+ `f28f160a`, `d9eee449`), docs `7d9bedc5` + `04a09b2b`,
  x86_64 resume-lane repair + t42 `e4f6c0bb`.
* **Baseline B** = `6ba03af9` (RISCV-ECALL-A7-RET1, the canonical dev head), worktree `ea7-wt`, clean.
  B has no timed wait (NR 9 bit 1 is reserved there: `InvalidArgs`), so the FUTEX-TIME1 tests and
  witness have no baseline reproduction; B is the comparison for every inherited suite.
* **Runner:** `ft1_chain.sh`: (1) hosted negative controls at F (`ft1_nc_run.sh` + `ft1_nc.py`, the
  FULL hosted lib per control, 28 controls and one unmodified run); (2) live (`ft1_live.sh`);
  (3) the matrix (`ft1_matrix.sh`). Sequential; every step runs whatever earlier steps returned; no
  retries in place; a rerun is appended, never substituted, and declared here first; logs, rcs,
  durations, HEAD, porcelain and every `ARTIFACT_BINDING` kept under `ft1-evidence`. No step sets
  `CARGO_TARGET_DIR` for the QEMU artifact scripts (they publish from `./target`; see the deferred
  defect of 2026-11-13).
* **Development runs before the freeze** (kept, not qualification): `ft1-live/dev-*` — r1 invalid
  (`CARGO_TARGET_DIR` override, stale binaries), r2 refused by artifact binding (source edited
  mid-build), RISC-V r3/r4 E not saturated (fillers lacked TLS; then E's deadline too short), r5
  pass; AArch64 r1 pass; x86_64 r1 FAILED (the resume-lane defect, repaired in `e4f6c0bb`), r2 pass.

## Expected

### (1) Hosted negative controls — each must fail >= 1 permanent test BY BEHAVIOUR

| id | defect reintroduced (brief §16 item) | expected to fail (at least) |
|---|---|---|
| nc1 | a wake of a timed cycle publishes TimedOut | t08 |
| nc2 | the timeout publishes Woken ("wake success after timeout won") | t09, t36–t38 |
| nc3 | the timeout claim does not retire (duplicate timeout collection) | t31, t39 |
| nc4 | a wake of a timed cycle publishes no completion | t08, t23 |
| nc5 | an already reached deadline parks | t03, t07 |
| nc6 | the timeout steals an NR 17 Pending wake | t17 |
| nc7 | the collector times out waits that are not due | t05 |
| nc8 | capacity refused, claimed anyway (retired before placement is secured) | t13 |
| nc9 | ticks without the partial-period `+ 1` | t34 |
| nc10 | a retired timed record is not uncounted | t40 |
| nc11 | the arm does not advance the wait generation | t39 or t26/t27 |
| nc12 | the split route precommits `1` into a timed parked frame | t22/t23 or the parked-answer guards |
| nc13 | the resume boundary never consumes a FutexWait completion | t36–t39 |
| nc14 | the collector cursor never advances | t32 |
| nc15 | the timeout read without FUTEX_FLAG_TIMED | t01 |
| nc16 | the deadline wraps instead of refusing | t02 |
| nc17 | the timeout claim by bare TID (no token/status check) | t11, t26 |
| nc18 | expiry leaves the record on its user word (wake and timeout both claim) | t13, t14 |
| nc19 | the scan window unbounded | a bound test if one exists — may survive (reported) |
| nc20 | the deadline attached after the registration lock is released | t22/t23 |
| nc21 | a timeout lost when its home queue is full | t13, t14 |
| nc22 | cancel by the original key after requeue (timed waiter left on the source) | t15, t18, t19 |
| nc23 | resurrect an exiting task | t24, t25 |
| nc24 | an unplaceable due waiter skipped forever | t14 |
| nc25 | the wrong parked result on RISC-V (`a0` success whatever ended it) | t36 |
| nc26 | the correct timeout result lost during context restore | t36–t39 |
| nc27 | clock progress stops when the system is idle | t33, t34 |
| nc28 | x86_64: the completion reaches the fields but not the restored registers | t42 |

Any control that survives is reported as such, with the test gap, and is not counted toward the
brief's 15. The unmodified run (`F_control`) must pass everything.

### (2) Live, SMP 2

* **LT-<isa>-<n>**, n = 1..3, ISA = riscv64, aarch64, x86_64: `FUTEX_TIME_WITNESS_SEAL result=ok`
  (grader rules in `scripts/grade-futex-time-witness.py`: A/F timeouts with every other cell
  thread blocked and >= 1 collected on an idle CPU; B, C, D as specified; G >= 1 window that held a
  wake from another CPU, >= 1 wake-won and >= 1 timeout-won cycle; E a SchedulerFull refusal, an
  `EXPIRED_UNPLACED` of the waiter, then its TimedOut placement). A failing run is a failed run.
* **L-nc2** (RISC-V) and **L-nc28** (x86_64): must FAIL the grader (ring 3 sees `woken` for a
  kernel TIMEOUT; ring 3 sees `error` for every switched resume).
* **LW-<isa>**: the wait witness (FUTEX-WAIT-ATOMIC1 / RISCV-PARK-RET1) passes, `parked_err = 0`.
* **LE**: the RISC-V ecall register witness passes (RISCV-ECALL-A7-RET1).
* **LS-<isa>**: the IRQ saturation witness passes. (FUTEX-WAKE-CAP1 had no live witness script; its
  live futex-saturation coverage is sequence E.)

### (3) The matrix — as RISCV-ECALL-A7-RET1's (`ea7_schedule.md`), with F and B substituted

P0/P1 provenance and source shape; H1 fmt; H2/H2b clippy and classes vs B; H3 hosted lib; H4
integration; H5 subsets (adds `time1`); H6 lock order per (test, pair) vs B; G1/G2 ABI contract
gate on F and B; G3/G4 marker-scan and grader self-tests (adds the time grader); D1–D3 default
builds; O1–O3 recv-v2 oracles; UN damaged-marker control; S1–S7 strict builds and core smokes, CI
timer grep; FX1 first-cohort seal; W IPC-DISPLACE1 SMP1 ×3; FX2–FX6 overtaken ×2, AP scheduling,
KTHREAD1, SCHED-LOCK1 ×3; FX7/FX8 FUTEX-REQUEUE1 broadcast and race ×3; R1–R3 RISC-V D2 settle, NR 1
source-fault and queued-cap witnesses; FB0–FB3 baseline strict/seal/overtaken; U1–U3 UART/PL011
IRQ; M1–M4 direct matrix, server-dies ×3; C1–C5 CI profiles, reply-timeout matrix, SMP-request on F
and B; AP marker integrity; X/FP sections and footprint F vs B; Z marker scan.

Expected: 0 failed anywhere F differs from B; the six baseline debts, d2-send and the idle-oracle
variance stay separately classified and are identical on F and B; nothing new is classified as debt
without the same failure on B.

## Addendum A (declared 2026-11-13T10:30Z, before any further step ran)

The container restarted at about 10:26Z, during nc4 (nc1–nc3 had recorded results; nc4 had none).
Rule from here on, for every phase: a step that has no recorded `rc` because the container
restarted is rerun once, from that step; its partial log is kept with the suffix `.interrupted`;
a step with a recorded result is never rerun. The runners are made resumable on exactly that rule
(`ft1_resume.sh`): the NC runner continues from the first control without a summary line, the live
runner and the matrix skip labels whose result line exists. Nothing else changes.

## Addendum B (declared 2026-11-13T11:40Z, while live ran; before the matrix)

The hosted negative controls at F showed test gaps, not defects: nc11, nc13 and nc26 survive the
full hosted lib; nc17 is caught only by a source-structure census guard, and on reading its mutation
is masked by `timeout_claimable`'s live check, so it is not a faithful bare-TID control; no control
covered the brief's "deadline left on a recycled slot". Response, test-only (no production change):

* new behavioural tests in `time1` for: the in-lock resume take (`take_resume_completion`) and the
  thread-restore facts each delivering a timed cycle's completion exactly once; a stale completion
  of an earlier cycle never matching a later cycle (the wait generation); a timeout candidate from
  a recycled slot / reused numeric TID claiming nothing;
* new controls nc29 (the timeout ignores the token's generation: claim by slot and bare TID) and
  nc30 (the stale-cycle variant of nc11 if needed); nc17 is reported as structural-only and not
  counted;
* candidate F' = F + those test commits; the kernel binaries are byte-identical (the tests are
  `cfg(test)`), checked by comparing the default and witness kernel images' sha256 F vs F';
* the full hosted NC run (nc1–nc30 + control) is repeated at F' and appended (`nc-Fprime`); the
  first run's results at F stay as recorded;
* the live phase continues at F (binary-identical to F'); the matrix waits for a gate file and then
  runs at F'.

### Addendum B, procedure (declared 2026-11-13T12:05Z)

* F' = `27956ed9` = F + one commit touching only `src/kernel/boot/tests.rs` (t43 in-lock resume
  take, t44 thread-restore facts, t45 cycle identity). Full hosted lib at F': 6605 passed, 0 failed.
* nc29 added (a retired timed record's deadline kept with its slot and taken by the next
  registration there). Brief mapping: "cancel a deadline by bare TID" is nc11 (the cycle identity
  reduced to TID + ASID: the generation never advances), caught by t45; nc17 is reported as
  structural-only and not counted.
* After the live phase ends: `wip/futex-time1` is fast-forwarded to F' in `ft1-wt` and pushed;
  the RISC-V time-witness kernel and initramfs are rebuilt at F' and their sha256 compared with
  LT-riscv64-1's (`sha256.txt`) as the binary-identity check; the NC run is repeated at F'
  (`OUT=ft1-evidence/nc-Fprime`, nc1–nc29 + control); then `ft1_matrix.go` is created and the
  matrix runs at F' (its P0 reads `ft1_fprime`).

## Addendum C (declared 2026-11-13T13:13Z, after the live phase; before NC-F', the gate, the matrix)

The live phase at F ended (`ft1-evidence/live/summary.txt`, LIVE_DONE). Its results stand as recorded:
LT 8 of 9 pass; **LT-x86_64-1 FAILED** (call 41, E, never returned); L-nc2 and L-nc28 fail the grader
as required; **LW-riscv64, LW-aarch64, LW-x86_64 and LE FAILED to build**; LS ×3 pass.

1. *LT-x86_64-1 — attributed, inherited, not repaired here.* E's timeout was claimed on CPU 1 with
   its reservation GRANTED (no `FUTEX_TIMEOUT_EXPIRED_UNPLACED`, no
   `FUTEX_WAKE_RESERVED_COMMIT_REFUSED`): the reservation took the one free slot of CPU 1's queue
   (`MAX_RUN_QUEUE` 64) while CPU 1's current task (filler 10042) was outside it. From then on CPU 1
   cannot rotate: 47 120 `YIELD_SPLIT_REFUSED reason=reenqueue_failed` and 416
   `TIMER_SPLIT_PREEMPT_DEFERRED reason=reenqueue_failed settlement=continue_current`; W, H and every
   task queued on CPU 1 are never dispatched. The scheduler has no rotation room: a re-enqueue of the
   current task is a user push under the same 64 limit, and `on_preempt_selection` /
   `preempt_reenqueue_only` keep `current` when it is refused. F changes none of
   `scheduler.rs`, `yield_txn.rs`, `scheduler_timer.rs`, `task_enqueue.rs`,
   `spawn_reservation.rs`. Reproduced deterministically at **B** with an UNTIMED `FutexWake` into the
   last free slot (scratch test, never committed; `ft1-evidence/addC/repro_B.log`), and at F' with
   both the untimed wake and t14's timed-out placement (`repro_F.log`). Classified as an inherited
   scheduler defect, reported separately from the six baseline debts, d2-send and the idle-oracle
   variance; recorded in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`. The run stays FAILED. No repair
   in this stage (scheduler policy, outside FUTEX-TIME1).
2. *LW / LE — a defect of F.* `futex-wait-witness`'s `time_witness`-unrelated outcome marker
   (`src/kernel/boot/futex.rs` `wait_witness::result`) matches `FutexParkOutcome` without the new
   `TimedOut` arm (E0004); the feature was never compiled by the hosted lib or the default builds.
   Response: a check-compile sweep of every kernel feature per ISA at F' and at B
   (`ft1_featsweep.sh`, `ft1-evidence/featsweep-{Fprime,B}`); every failure present at F' and not
   at B is repaired in ONE commit (feature-gated code only) giving **F''**; failures identical at B
   are inherited and reported. Default and time-witness kernels must be byte-identical F vs F''
   (sha256 of the RISC-V and x86_64 time-witness kernel + initramfs vs LT-riscv64-1 / LT-x86_64-1,
   and the default kernels), which keeps the LT/L-nc/LS evidence at F valid for F''.
3. *Appended live runs at F''* (the first runs stay as recorded): LW-<isa>-C ×3 ISAs, LE-C, and
   three further x86_64 time-witness runs LT-x86_64-4..6 (observation of the x86 rate; they do not
   replace run 1).
4. The NC run at F' (Addendum B) is done at F'' instead (`nc-Fpp`, nc1–nc29 + control); the gate
   `ft1_matrix.go` is created only after it, with `ft1_fprime` = F''.

### Addendum C, procedure (declared 2026-11-13T13:35Z)

* Sweep result (`ft1-evidence/featsweep-compare.txt`): F' 132 checks, B 129 (F' adds
  `futex-time-witness` ×3, all pass). The ONLY F'-fails/B-passes entry is `futex-wait-witness` on
  all three ISAs. 22 umbrella-feature checks (`kstack-probe*`, `kthread1-witness`, `lock-witness`,
  `sched-lock1-witness`, `apsched-witness` on x86_64) fail identically on B: inherited, reported.
* **F'' = `fb140666`** = F' + one commit, `src/kernel/boot/futex.rs` `wait_witness::result` gains
  `O::TimedOut => "timed_out"` (inside `#[cfg(feature = "futex-wait-witness")]`); the three
  `futex-wait-witness` checks now pass (`addC/fix_check_*.log`). `wip/futex-time1` fast-forwarded
  and pushed; `ft1_fprime` = F''.
* Chain (`ft1_chainC.sh`, sequential): `ft1_liveC.sh` (ID-<isa> identity, LW-<isa>-C, LE-C,
  LT-x86_64-4..6) → NC at F'' (`nc-Fpp`, nc1–nc29 + control; resumes from the first control with no
  summary line) → `touch ft1_matrix.go` (the waiting matrix runs at F''). A DIFFERENT identity
  result does not stop the chain; it is reported, and the LT/L-nc evidence at F is then not
  carried to F''.

## Addendum D (declared 2026-11-13T16:05Z, while the matrix ran at F''; before reading any matrix result)

Addendum C (2) asked that F'''s byte-identity keep the live evidence at F valid. `ft1_liveC.sh`
checked the time-witness kernels and initramfs (ID-<isa>, identical on all three ISAs); it did not
check the IRQ saturation witness kernels used by LS-<isa> at F. The matrix runs at F'' itself, so
its default, strict and other witness builds need no identity argument; LS is the only result
carried from F that the ID check does not cover. Appended after MATRIX_DONE (`ft1_liveD.sh`,
sequential, resumable by label, no retries): **LS-<isa>-D** ×3 at F'' — the same command as LS-<isa>
— with its `ARTIFACT_BINDING kernel_sha256` compared with LS-<isa>'s at F (`identical` or
`DIFFERENT`). A DIFFERENT kernel is reported and the LS result at F is then not carried to F''; the
LS-<isa>-D result is reported in its own right either way. The LS runs at F stay as recorded.

## Addendum E (declared 2026-11-13T18:05Z, after reading FX1, FB1, S7, FX6 while the matrix ran; before any rerun)

Read so far (the matrix still running at M1): every step rc 0 except
* **FX1** (F'') and **FB1** (B) — rc 1, identically: `[seal][fail] aarch64 FutexWait idle oracle proof
  missing`, 12/12 live cells on both. Classified as the inherited idle-oracle variance (present on
  B in the same run). No rerun.
* **S7** — rc 1, 0/0/0: inherited #6, as in RISCV-ECALL-A7-RET1's matrix. No rerun.
* **FX6_schedlock1_riscv64** (F'') — rc 1: `SL1W_GRADE result=fail reason=progress: round 0:
  PROGRESS seq 22's installation seq 18 is not the observer's own commit inside the hold, before a
  later tick`. CPU 1 installed its task (seq 18) two records before CPU 0's `HOLD_BEGIN` (seq 20)
  and it stayed current through the hold, so every PROGRESS of the hold names a pre-hold
  installation. FX6 on x86_64 and AArch64 passed; the matrix runs FX6 only at F''. F'' changes no
  scheduler, witness or grader code on this path; the new trap-tail collector is one relaxed load
  with no timed waiter. That is an argument, not a reproduction.

Appended after LIVED_DONE (`ft1_addE.sh`, sequential, resumable by label, no retries): the matrix's
exact FX6 command (`ARCH=riscv64 SMP=2 scripts/qemu-sched-lock1-witness-smoke.sh`), interleaved
**FX6E-B-1, FX6E-F-1, … FX6E-B-5, FX6E-F-5** (B in `ea7-wt` at `6ba03af9`, F'' in `ft1-wt`).
Classification, fixed now: if ANY B run fails the grader on the same rule (`reason=progress`, an
installation not inside the hold), FX6 riscv64 is an inherited witness variance, reported
separately from the six debts, d2-send, the idle-oracle variance and the last-slot starvation. If
no B run fails that way, the F'' failure is NOT attributed: it stays unexplained, and the verdict
cannot be ACCEPTED(_WITH_BASELINE_DEBT) on it without further, separately declared work. The first
FX6 result stays as recorded either way.

## Addendum F (declared 2026-11-13T19:10Z, after reading the whole matrix, Addendum D and Addendum E; before any further run)

Read: the matrix ended (MATRIX_DONE). Beyond Addendum E's reading — M1–M4, C1, C3, X, Z rc 0; C2/C4
fail the same four profiles on F'' and B (d2-send, smp2/smp4-sender-wake, sched-timeout: d2-send
and inherited #2–#4); C5a/C5b identical (#1, `class=IpcReplyTimeout`); C5c/C5d identical (#5, line
67); AP_marker_integrity rc 1 on ONE log only, `UN/x86_damaged.log`, the damaged-marker control's
deliberate split (RISCV-ECALL-A7-RET1 Addendum C's scope error, recurring). Addendum D: LS-x86_64-D,
LS-aarch64-D pass, kernels identical; **LS-riscv64-D FAILED** (`delivered=15, expected 16`) on a
kernel byte-identical to LS-riscv64's passing one. Addendum E: FX6E-B-1..5 and FX6E-F-1..5 all
pass — so, by E's rule, FX6's F'' failure is not yet attributed.

Mechanisms read from the logs and the code (not yet a reproduction):
* *LS-riscv64-D:* the boot log has 16 kernel `outcome=delivered` lines; one is
  `IR` + newline + the host driver's own `#HOST_INJECT byte=0x67 … claim=14` + newline +
  `Q1_SPLIT_DELIVERY …`: the smoke script's injection annotation, written into the same log file,
  split a kernel marker. The grader fails closed (correct). Script and grader unchanged since B.
* *FX6 riscv64:* `schedlock1_witness.rs` (unchanged since B) reserves a section's `SEC`/`ADD`/`REM`
  record numbers first and only at the end of the same function checks `open_hold()` to count an
  observer OWNER COMMIT into `B_INSTALLED`. If the holder records `HOLD_BEGIN` and stores
  `HOLD_ACTIVE` between those two points, a section whose records precede `HOLD_BEGIN` is counted
  as an in-hold installation, and the observer's later `PROGRESS` names it — exactly seq 18/19
  (CPU 1) < 20 (`HOLD_BEGIN`, CPU 0), then PROGRESS seq 22 → iseq 18.

Appended (`ft1_addF.sh`, sequential, resumable, no retries; scratch commits in the detached
`ft1live-wt`, never pushed):
1. **FX6F-Bd-1..3** — B + a scratch delay (`ft1_addF_delay.py`: 1 000 000 `spin_loop`s in that
   function, after the records are reserved and before the `open_hold()` check, only for a section
   on its own CPU that installs a task while no hold is open); **FX6F-Fd-1..3** — F'' + the same
   patch.
2. **FX6F-B-1..10 / FX6F-F-1..10**, unmodified, interleaved (B `ea7-wt`, F'' `ft1-wt`).
3. **LSF-B-1..3 / LSF-F-1..3**, the LS riscv64 command unmodified, interleaved; each failing run is
   scanned for a `#HOST_INJECT` line splitting a kernel marker.
Classification, fixed now. *FX6:* if ≥ 1 FX6F-Bd run fails with `reason=progress` naming an
installation whose `SEC` precedes the hold's `HOLD_BEGIN`, the race is present at B: FX6 riscv64 is
an inherited witness race, reported separately (recorded in `KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).
Otherwise it stays unattributed and the verdict is `FUTEX_TIME1_DEFERRED`. *LS-riscv64-D:*
classified as a harness log interleaving if the damaged marker is a host-written line (it is,
above) — the LSF runs are observation only; any LSF failure is reported with its own mechanism.

## Erratum (appended 2026-11-13T18:50:03Z, read from the clock)

The "declared" times in the headings of Addenda D, E and F were estimates written by hand, not read
from the clock, and F's ("19:10Z") is later than its own runs (`addF start 2026-11-13T18:48:06Z`).
The ordering that matters is the one the files record: each addendum's text and runner were hashed
into `predeclared.sha` (lines `addD`, `addE`, `addF`) before its runner was started — D before
`liveD start 2026-11-13T18:34:22Z`, E before `addE start 2026-11-13T18:39:29Z`, F before
`addF start 2026-11-13T18:48:06Z` — and each was written after the results it cites and before any
run it schedules. The addenda's text is not edited (its hashes stand).
