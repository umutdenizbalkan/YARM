
#### PRE-SCHEDULER-CLEANUP1 (from accepted `2866be83`)

One focused cleanup before scheduler development, on `wip/pre-scheduler-cleanup`. Four parts,
each its own reviewable commits; one frozen candidate F = `da7bb379`, qualified once against
B = `2866be83`. Scheduler policy, the scheduler lock design, the clock source and the vDSO are
untouched; ABI v12 is unchanged.

**1 — SR-PLACE1 (shared-region receiver placement).** On B, `sr_wake_receiver_split` (and its broad
twin) made the receiver `Runnable` under the task lock and then tried a refusable enqueue whose
`QueueFull` it discarded. Reproduced on B through the real delivery path (`sr_place1` r1–r3 fail on
`2866be83`): on a full home queue the production direct path's finalize rolled back a delivery the
sender had already been told succeeded (`complete_ok` precedes the drain), so the message was lost;
the plain branch left the receiver `Runnable` on no queue; and it rewrote a RUNNING receiver to
`Runnable`. Now both direct producers reserve the receiver's home slot (FUTEX-WAKE-CAP1's
`reserve_enqueue_on_cpu_locked`, under that CPU's 1L only) before anything is taken — a refusal
returns `WouldBlock` with nothing taken (`SR_RECEIVER_PLACEMENT_REFUSED`), and the sender retries;
the promise travels in the delivery snapshot; the finalizer re-ensures it before its claim and commits
it after (`SrPlacementCommit::{Queued, AlreadyPlaced, Refused}`); every rollback releases it; a
`Running` receiver keeps `Running` and its promise is released; an `AlreadyPlaced` collision fails
closed like Stage 199D's. One delivery therefore produces exactly one receiver completion. Tests
`sr_place1` r1–r9 (refusal with nothing taken and later recovery, the promise holding the slot, stale
waiter identity, exit during delivery, already-placed receiver, map-fault rollback releasing the
promise), the Stage 199D and 198E3B2B suites, and negative controls srnc1–srnc6 (each removing one
part of the repair) — every control caught.

**2 — PRINTK-FOOTPRINT1.** The ring's slots moved to an all-zero static (`PrbSlot::new()` had set
`level = Debug`, which put the whole array into `.data`); the default ring is 64 records — 128, 64
and 32 were each measured lossless on the default and strict core smokes (x86_64 SMP 1/2/4, AArch64
SMP 2, RISC-V SMP 1/2/4, strict on all three) and 64 is the structural floor `MAX_CPUS` (asserted);
the marker-heavy witnesses keep 1024 through `printk-witness-ring`. Matched default release
artifacts, B → F:

| ISA | `.text` | `.rodata` | `.data` | `.bss` | boot image | loaded memory (PT_LOAD memsz) |
|---|---|---|---|---|---|---|
| x86_64 | +3,984 | +152 | −213,016 | +13,112 | −208,400 | −195,744 |
| AArch64 | +5,696 | +184 | −212,984 | +12,288 | −204,792 | −186,632 |
| RISC-V | −970 | +184 | −212,984 | +13,278 | −217,080 | −180,026 |

`.bss` grows by the 64-record backing (64 × 208 B). The `.text` growth that SCHED-TIME-FOUNDATION1
recorded (x86_64 +20,284 B, 8062c216 → 2866be83) attributes by symbol to code, not duplicated
diagnostics: `split_futex_park_tail` +5,918 (extracted from NR 9 and shared with NR 20 — one copy),
`handle_trap_entry_shared` +4,879 (inlining), `run_due_ipc_timeout_work` +2,036, the clock syscalls
~3.8 KB, timekeeping ~2 KB, Debug formatting a few hundred bytes; the `TIMEKEEP_*` and
`*_RESERVED_COMMIT_REFUSED` pairs are variants (broad/split twins), so nothing was removed. This
stage's own `.text` delta is SR-PLACE1's reservation path.

**3 — scheduler qualification evidence.**

* *SCHED-LOCK1-ORDER1.* The holder publishes its `HOLD_BEGIN` record number before the hold opens;
  `before_release` credits an observer commit only when the section's first record follows it
  (`schedlock1_hold_order::credited_in_hold`, hosted-tested on the boundary 18/19/20/21/22), the
  grader's own rule; the grader is unchanged in strictness and its self-test gains the defect's
  shape. Live, with a scratch delay (never committed) holding an observer section at the boundary
  until a hold opens (RISC-V SMP 2, six runs each): B failed exactly the runs that reached it
  (2 of 6 — producer `B_COMMITS` 2 against 1 in-hold commit, round uncredited); F passed 6 of 6,
  4 at the boundary, `B_COMMITS` equal to the in-hold count in every hold.
* *SCHED-SAT-EVIDENCE1.* Saturation is proved by the kernel at the refusal: `reserve_enqueue_on_cpu_locked`
  records, under the home CPU's 1L, that queue's entries and promised slots at the waiter's priority
  (`kernel::sat_witness`, witness builds and hosted tests only), and the refused owner logs
  `SATW_REFUSED class=sleep|futex|recv … queued reserved cap full facts n`. The sched-time cell adds
  thread R — calibrated on the port's tick, kept runnable until H reports the queues full, then a
  short timed IPC receive on init's idle PM reply endpoint; the spawn pool is shared and the fillers
  top up every 250 ms during the hold (H gets the CPU only once per rotation, tens of seconds under
  RISC-V TCG saturation); whichever cell thread passes the published end releases. FT1 E tops up
  through its ping hold. The graders require, per waiter, a full home-queue refusal at its deadline
  logged before the release, then exactly one recovery: W's one DONE; R's deferral of that wait
  generation and exactly one settlement before R returns its timeout; FT1 W's one EXPIRED then one
  TIMEOUT. Spawn refusals, filler counts and other threads' deferrals earn nothing. Development took
  seven live iterations (each recorded in its commit message): a receive blocked before saturation
  could not be woken onto a full queue; tick deadlines slow down ~1.4× in wall time under saturation;
  R's own filler went to the other CPU; a sparse filler check missed R's window; H-driven release
  waited 700 s for H's turn.
* *ARTIFACT-CARGO-BIND1.* Every kernel and server ELF the artifact scripts publish is the
  executable the current `cargo build` reported in its JSON messages; a publish refuses any other
  binary; the initramfs record checks every packed member against it; records are version 2 and the
  boot guard refuses older or unbound ones; `CARGO_TARGET_DIR` works. Regression
  `scripts/test-artifact-cargo-binding.sh` (real cargo build into an alternate target directory with a
  stale binary at the old path), run by `tests/artifact_binding_scope.rs`.

**4 — Qualification (F = `da7bb379` vs B = `2866be83`).** Runner and helpers hashed before the first
step (`predeclared.sha`), two addenda declared and hashed before they ran; nothing rerun in place;
every log kept.

| phase | result at F |
|---|---|
| hosted | fmt; clippy (424 warnings vs 427 at B; one new class instance — `ReceiverEnqueue` lost its last production consumer to SR-PLACE1, lint only); lib 6,686 tests serial; integration; 18 targeted subsets, each > 0 tests (sr_place1 9, stage199d 427, stage198e3b2b 38, shared_region 178, sat_witness 4, schedlock1 2, printk 8, sched_full_rot1 15, sched_time_foundation1 16, sleep1 17, futex_requeue1 182, futex_wake_cap1 14, ipc_timeout 4, census 33, lock_order 4, broad 152, abi 271, syscall_abi 2); contract gate; five grader self-tests; the artifact-binding regression; marker-scan self-test — all pass |
| lock order | the same 30 descending-pair types on F and B (the hosted tracker logs sequential rank decreases, not nesting); deltas only in the new `sr_place1` tests and the shared-region tests' added reserve/commit steps (`scheduler` after `task`), plus a multi-threaded race test's interleaving; no nested acquisition, no new broad-lock or all-CPU acquisition |
| builds and smokes | default x86_64 / AArch64 / RISC-V builds and SMP 2 core smokes; strict builds and core smokes on all three — all pass |
| artifact binding live | RISC-V default build with `CARGO_TARGET_DIR` set elsewhere while `./target` held a different (witness) kernel: the published kernel is the alternate directory's output (`97550b95…`), not the old path's (`f5dddf17…`); record v2 with its Cargo binding and bound initramfs members; the core smoke boots it |
| SCHED-LOCK1 witness | x86_64, AArch64, RISC-V SMP 2 — all pass |
| FUTEX-TIME1 witness | x86_64, AArch64, RISC-V — sealed; E proved on W's own full home queue each time |
| sched-time witness | x86_64, AArch64 sealed (W and R each proved full at their deadlines; R settled exactly once); RISC-V **failed twice** (W3, W3b — premise not reached, below) |
| shared-region delivery witness | W1 (all three) refused before QEMU — runner error, see below; W1b (Addendum A) x86_64, AArch64, RISC-V — sealed (`installs=2 misplaced=0`, `fuse_trips=0`) |
| markers | no failure marker in any F log; no `PRINTK_RECORDS_LOST` |

Non-zero steps, each classified: `H2b_clippy_classes` (above, lint only). `W1_shared_region_*`: the
three scripts require `initramfs:shared-region-offset-witness` unconditionally (since `15114160`,
unchanged on F), so the matrix's default `OFFSET_SELECTION=0` invocation was refused by the boot guard
on every port; Addendum A reran them with `OFFSET_SELECTION=1`. `W3_sched_time_riscv64`: the cell's
premise guard failed (saturation completed at 153.6 s, after W's 149.6 s deadline; dev runs saturated
at 56–80 s), while the kernel-side proofs in that same run held (W 4 proving refusals, R 4 with one
settlement). Addendum B reran it once (`W3b_sched_time_riscv64`, rc=1): saturation completed at 175.5 s
against W's 145.8 s deadline, so W was never refused; R's home queue held 63 of 64 at its deadline and
R's timeout settled at once — the only `recv` refusal was a duplicate publication drained on the other
CPU while R's own promise was outstanding, logged after the settlement, and the grader rejected it.
Every guard failed closed; in both runs W completed once and R settled once and returned once. The
baseline has no saturation-proving cell, so a baseline rerun would not resolve attribution; it was not
run. The cause is the cell's fixed W deadline against a variable saturation time under RISC-V TCG
(ledger, OPEN LIMITATION); the remedy would change the frozen candidate and was not applied.

**Verdict: NOT QUALIFIED as delivered** — every kernel, build, footprint, binding and witness check
passed on all three ports except the RISC-V sched-time witness, whose saturation-at-deadline condition
was reached in development (ST-rv-6) but in neither qualification run. The work branch carries F and
this record; `claude/yarm-kernel-unlock-u0-f2zh8p` stays at `2866be83` (not fast-forwarded). Scheduler
design inputs from this stage are in `SCHEDULING_CONTRACT.md` §19.
