**Qualification** (frozen candidate F'' = `fb140666`, clean tree, against baseline `6ba03af9`,
clean tree; schedule written before the run and kept with its sha256; six addenda (A–F), each
hashed before anything it schedules ran, and an erratum (the addenda's heading times were hand
estimates; the hash order against the runs' logged start times is what is kept); sequential, no
retries in place; every log, rc, duration, HEAD, porcelain and `ARTIFACT_BINDING` kept). Addendum A
made the runners resumable after a container restart (nc4 at F; its partial log kept); B added the
test commit `27956ed9` and nc29; C the attributions above and F''; D–F below.

| cell | result |
|---|---|
| provenance; source shape | HEAD `fb140666`, porcelain 0, both trees |
| fmt; clippy (hosted) | rc 0; 425 warnings, classes identical to `6ba03af9` |
| hosted lib, serial; integration | 6605 passed, 0 failed (3 ignored); 41 binaries, 396 passed, 0 failed (incl. `broad_lock_census_guard`, `doc_fragmentation_guard`) |
| 31 subsets (`time1` … `sender_wake`) | 0 failed each |
| lock order per (test, pair), 21 619 common tests | only `c09_reply_vs_timeout_terminal_cas_race` (the known run-to-run variant); `FUTEX_WAKE_STOPPED` in the same tests on both; the 481 new tests add pairs only of ranks already seen |
| contract gate (ABI v11) | rc 0 on both |
| marker-scan, wait / requeue / ecall / time grader self-tests | all cases hold |
| default builds ×3; recv-v2 oracles ×3; damaged-marker control; strict builds and strict core smokes ×3 | rc 0 |
| first-cohort futex seal, candidate and baseline | both FAIL identically: 12/12 live cells, `aarch64 FutexWait idle oracle proof missing` — the inherited idle-oracle variance, recurring on both trees in the same run |
| CI strict timer grep | 0/0/0, inherited #6 |
| IPC-DISPLACE1 SMP1 ×3; overtaken ×2 (both trees); AP-scheduling, KTHREAD1 SMP2 ×3 | pass |
| SCHED-LOCK1 SMP2 ×3 | x86_64, AArch64 pass; **RISC-V FAILED** — `progress: PROGRESS seq 22's installation seq 18 is not the observer's own commit inside the hold`: the inherited witness race (below) |
| FUTEX-REQUEUE1 ×3; FUTEX-REQUEUE-ATOMIC1 race ×3 | pass ×3 each |
| RISC-V D2 settle (both trees); NR 1 source-fault send and queued cap-bearing reply ×3 ISAs (RISC-V also on `6ba03af9`) | rc 0 |
| baseline strict builds, seal, overtaken ×2 | as on the candidate |
| UART / PL011 IRQ witnesses ×3; reply-direct matrix; server-dies ×3 | rc 0 |
| CI profiles | identical: sender-wake, smp2-core, smp4-core pass; d2-send and inherited #2, #3 (`D6_SMP_DISPATCH_OK`), #4 (no idle-entry timeout) fail on both |
| reply-timeout matrix; SMP-request script | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67), identical on both |
| AP marker integrity | rc 1 on ONE log, the damaged-marker control's deliberately split replay (`UN/x86_damaged.log`, the scope error RISCV-ECALL-A7-RET1's Addendum C recorded); every other candidate and baseline log 0 split markers, 0 ladder bytes, 0 raw TTBR0 bytes |
| sections and footprint | below; marker scan 0 hits |

*Appended (Addenda D–F).*
* *LS-<isa>-D* (Addendum D) — the IRQ saturation witness at F'': kernels byte-identical to LS-<isa>
  at F on all three ISAs; x86_64 and AArch64 pass; **LS-riscv64-D FAILED** (`delivered=15, expected
  16`). The boot log has all 16 kernel deliveries; one marker is split by the smoke script's own
  `#HOST_INJECT … claim=14` annotation, written into the same log file (`IR` / host line /
  `Q1_SPLIT_DELIVERY …`), so the grader failed closed. The same split occurs on `6ba03af9`
  (LSF-B-3: three host lines inside guest lines, on ungraded markers); LSF-B-1..3 and LSF-F-1..3
  pass. Classified as a harness log interleaving (`KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).
* *SCHED-LOCK1 RISC-V* (Addenda E, F) — unmodified: `6ba03af9` 0 failures in 15 runs, F'' 1 in 16
  (the matrix's). Mechanism, in `schedlock1_witness.rs` (identical at both commits):
  `before_release` reserves a section's records before it checks `open_hold()` and counts the
  observer's installation into the hold; a hold opened in between makes a pre-hold installation
  count as the observer's own commit, and the grader then rejects a correct trace. Forced on
  `6ba03af9` with a scratch delay between the reservation and the check (never committed): 1 of 3
  runs failed in the same shape (observer SEC/REM seq 20/21, `HOLD_BEGIN` 22, `PROGRESS` 24 → 20);
  F'' with the same delay 0 of 3. Classified, by the rule declared before those runs, as an inherited
  witness race, separate from the six debts, d2-send, the idle-oracle variance and the last-slot
  starvation (`KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).
