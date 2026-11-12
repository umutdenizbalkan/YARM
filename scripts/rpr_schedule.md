# RISCV-PARK-RET1 — predeclared qualification schedule

* **Candidate F** = `194842bc`, the frozen head of `wip/riscv-park-ret1` (worktree `rpr-wt`, clean,
  pushed): repair `196f8b97`, tests `57e036aa`, guard wording `4c3d7b1b`, peer-gated witness
  `c30b0eba` (cherry-picked from FUTEX-WAIT-ATOMIC1's undelivered `ae65c57e`), witness/grader
  `ae82eef9`, a3..a5 assertions `35eb0f07`, docs `194842bc`.
* **Baseline B** = `2a91a6e8`, the canonical dev head (FUTEX-WAIT-ATOMIC1), worktree `fwa-wt`, clean.
* **Runner:** `rpr_chain.sh` runs, in order: (1) the hosted negative controls at F
  (`rpr_nc_run.sh`, `rpr_nc.py`, the FULL hosted lib per control, 10 controls and an unmodified run);
  (2) F's `riscv_park_ret1` suite verbatim on B with the one bridge step stood in for by B's
  behaviour, nothing (`rpr_baseline.sh`); (3) the live runs (`rpr_live.sh`); (4) the matrix
  (`rpr_matrix.sh`). Sequential; every step runs whatever earlier steps returned; no retries in
  place; a rerun is appended, never substituted, and declared here first; logs, rcs, durations,
  HEAD, porcelain and every `ARTIFACT_BINDING` kept under `rpr-evidence`.
* **Expected of (1):** NC1–NC9 each fail at least one permanent test by behaviour; NC10 (the
  structural control: a global lock on the settlement path) fails `t22`; the unmodified tree passes
  all. **(2):** on B, t02, t05, t06, t07, t08, t09, t19 and g05 fail (a parked wait resumes with
  `a0` = its address); the other 16 pass.
* **(3) live, SMP 2, the revised wait witness:** RISC-V ×3, then AArch64 ×3, then x86_64 ×3 — every
  run reported. Expected `witness=ok core_smoke=ok result=ok` with `parked_ok >= 2`, `parked_err=0`,
  `dup_or_missing=0`. A run that fails ONLY on a damaged marker is reported as such, with the
  damaging byte attributed (AArch64's `raw_uart_marker` is the known source) and counted as a failed
  run; it is not rerun in place and its log is not repaired. Acceptance needs all three RISC-V runs
  to pass and at least one clean run per comparison ISA with no `parked_err` in any run. Then the
  live controls: **L-NC2-riscv64** — F with NC2 (no commit) on RISC-V: must fail the grader with
  "GENUINELY parked … returned Err"; **L-NC2-x86_64** — the same patch on x86_64: must PASS (the
  commit is RISC-V-only; x86_64 resumes correctly without it).

| id | step | expected |
|---|---|---|
| P0 | provenance | HEAD=F, porcelain 0 |
| P1 | source: diff stat vs B; `src/arch/x86_64`, `src/arch/aarch64`, `src/arch/trap_entry.rs` unchanged; `riscv_commit_final_park_result` called once in production (the RISC-V bridge), before the capture | captured |
| H1 | `cargo fmt --all --check` | rc 0 |
| H2 / H2b | clippy hosted; class and count comparison against B | identical classes and counts |
| H3 | hosted lib, serial | 0 failed |
| H4 | integration binaries | 0 failed (incl. `broad_lock_census_guard`) |
| H5 | subsets: riscv_park_ret1, futex_requeue1::wait_atomic1, futex_requeue1::atomic1, futex_requeue1, futex_wake_cap1, futex_found1, futex, trapframe, async, riscv, u6, blocked, recv, send, robust, scheduler, sched_, enqueue, ap_sched1, stage199d, stage191b, stage191d, stage200d0a, stage29, syscall, u9qa, kthread, exit, reap, spawn, thread, fork, ipc_displace1, sender_wake | 0 failed each |
| H6 | lock order per (test, pair), F vs B, common tests (`riscv_park_ret1` skipped) | no change except the run-to-run variant `c09_reply_vs_timeout_terminal_cas_race` (the repair takes no lock; the witness is not in a hosted build) |
| G1 / G2 | contract gate on F / on B | rc 0 / rc 0 |
| G3 | marker scanner self-test | all cases hold |
| G4 | wait and requeue grader self-tests | ok |
| D1–D3, O1–O3 | default builds ×3; IPC recv-v2 oracles ×3 | rc 0 |
| S1–S6 | CI-exact strict builds + strict core smokes ×3 | rc 0 |
| FX1 | first-cohort futex seal | 12/12 live cells (idle-oracle variance tracked separately) |
| S7 | CI strict timer grep | inherited debt #6 |
| W | IPC-DISPLACE1 SMP1 ×3 | `witness=ok core_smoke=ok` |
| FX2–FX6 | overtaken ×2; AP-scheduling, KTHREAD1, SCHED-LOCK1 SMP2 ×3 | pass |
| FX7 / FX8 | FUTEX-REQUEUE1 witness ×3; FUTEX-REQUEUE-ATOMIC1 race witness ×3 | `result=ok`, `cross_cpu=yes`; `covered=3` |
| R1 | RISC-V D2 empty-current settle witness (SMP 1), F and B | F as B |
| R2 | NR 1 source-fault send witness ×3, F; RISC-V also on B | rc 0 on F, F as B |
| R3 | queued cap-bearing reply witness ×3, F; RISC-V also on B | rc 0 on F, F as B |
| FB0–FB3 | B: CI-exact strict builds, the seal, the overtaken witnesses | as on F |
| U1–U3, M1–M4 | IRQ witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| C1–C5 | CI profiles, reply-timeout matrix, SMP-request script, on F and B | identical; inherited #1–#5 and d2-send |
| X | x86_64 and AArch64 default kernels built at F and at B: per-section sha256 | `.text` identical on both (evidence that their execution is unchanged; `.rodata` may differ by source-line shifts) |
| Z | every QEMU log of F through `fwa_zscan.sh` | reported per file; any hit explained |

The six baseline debts, d2-send and the idle-oracle variance stay separately classified; nothing new
is classified as inherited without a reproduction on B.

## Addendum A (declared 2026-11-12 15:12Z, after FX7_requeue_x86_64 rc=1, before any rerun)

`FX7_requeue_x86_64` failed: `witness=ok cross_cpu=yes`, the standard core smoke rejected the boot on
the missing `X86_AP_KERNEL_CR3_BEGIN`, present in the log as `X86_AP_KEiRNEL_CRy3_BEGIN cupu=1` —
the AP trampoline's unlocked raw bytes `i`, `y`, `u` (`smp_trampoline.rs` 973–991) inside the BSP's
marker. The log is kept, not repaired; the step stays a failed run. Declared, appended after the
chain finishes (nothing substituted):
* **FX7r-F ×2** — the same command at F (`194842bc`), new LOGDIRs `FX7r1_x86_64`, `FX7r2_x86_64`.
* **FX7-B ×2** — the same command at B (`2a91a6e8`), `FX7B1_x86_64`, `FX7B2_x86_64`.
* **X7** — the futex-requeue-witness x86_64 kernel (`futex-requeue-witness` +
  `x86-shared-region-direct-oracle`, as the script builds it) at F and at B: per-section sha256;
  expected `.text` identical.
* **Mechanism on B** (already in hand): byte-split `X86_AP_*` markers in `FB1/x86_smp2.log`,
  `FB1/x86_smp4.log`, `FB2/boot.log` at B.
Classification rule: if X7 shows identical `.text` and no FX7r/FX7-B run fails on anything but a
byte-split marker, FX7_requeue_x86_64 is classified as the known x86 SMP serial interleaving
(KERNEL_UNLOCKING.md, recorded before at another stage's smp2-sender-wake), kept separate from the
six debts. Any other failure is reported as new.

## Addendum B (declared 2026-11-12 15:58Z, after reading X_text_identity.log, before any rerun)

`X_text_identity` returned rc 0, but its AArch64 half is VOID: every AArch64 section hashed to
`e3b0c442…` (sha256 of empty input) because GNU `objcopy` on this host cannot read an AArch64 ELF, and
the script did not refuse an empty extraction. The x86_64 half is valid (`.text` `718470dba4366aa1`
on both). The step's log is kept as run. Declared, appended after Addendum A finishes:
* **XB** — the AArch64 default kernel built at F and at B, compared per section with `llvm-objcopy`;
  an empty or failed extraction is a FAILURE (fail-closed); x86_64 recompared the same way as a
  cross-check. Expected `.text` identical.
* **FP** (footprint, measurement only) — the RISC-V default kernel at F and at B: `.text` size and
  the size of `riscv_commit_final_park_result` / `commit_riscv_parked_syscall_returns` (or their
  inlined absence) from `llvm-nm --size-sort`; reported, not graded.

## Addendum C (declared 2026-11-12 16:20Z, after reading XB and X7, before any rerun)

`XB_text_identity` rc 1: its fail-closed check refused every section as `EMPTY_EXTRACTION` — on this
host `objcopy`/`llvm-objcopy -O binary … /dev/stdout` into a pipe yields nothing for these ELFs (shown
on `qual/FX2/build/kernel_boot.elf`). `X7_text_identity` rc 0 is VOID for the same reason (every
section `e3b0c442…`; its script had no empty check). Both logs kept as run. `FP` succeeded (it reads
headers and symbols only). Declared, appended:
* **XC** — each section extracted with `llvm-objcopy -O binary --only-section=S <elf> <regular file>`;
  a non-NOBITS section whose extracted size differs from its header size, or a missing file, is a
  FAILURE; sha256 of the file compared F vs B. Kernels: the AArch64 and x86_64 default kernels as
  Addendum B left them in each worktree (built at F and at B, rc 0, `XB/provenance.txt`), and the
  x86_64 futex-requeue-witness kernel (base artifacts with `futex-requeue-witness`, kernel with
  `x86-shared-region-direct-oracle`, as `qemu-futex-requeue-witness-smoke.sh` builds it) at F and at B.
  Expected `.text*` identical for all three.
