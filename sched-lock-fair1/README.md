# SCHED-LOCK-FAIR1 evidence

Candidate `b7263d21` (`wip/sched-lock-fair1`), baseline `dce48596`. Design record:
`doc/SCHEDULING_CONTRACT.md` §22; stage record: `doc/KERNEL_UNLOCKING.md`, "SCHED-LOCK-FAIR1".

## How it was run

* `qual/predeclared.sha` hashes `fair1_qual.sh` (the runner), `fair1_mutlog.py` (its live-log
  mutation helper) and `fair1_addA.sh` (addendum A), each before it ran. All three are included in
  `scripts/`.
* `qual/summary.txt` lists every step in order, with the candidate head, the porcelain count and the
  exit status. Every step's log is kept under its own name, and nothing was rerun in place.
* The two raw `--nocapture` lock-order logs are gzipped. Built kernels and initramfs images are not
  included; their hashes are in `qual/kernels/SHA256SUMS` and in each live cell's
  `artifact-binding.txt`.

## Development runs before the candidate

`dev/` holds the development boots of the extended witness on the uncommitted tree (one per port,
SMP2, all `result=pass`). It also holds the first hosted run (`fair1_dev1.log`: 3 failures, all
fixed before the candidate) and the second (`fair1_dev2.log`), a `--workspace` run.

The second run has two failures, both outside the `yarm` package:
* `yarm-server-runtime`'s `runtime_reexports_curated_runtime_surfaces`;
* a `yarm-control-plane-servers` doctest.

Both fail identically on `dce48596` (`dev/fair1_base_workspace_fails.log`), so they are inherited.
The qualification runs the `yarm` package's suites, as the earlier stages did.

`dev/adhoc-analysis/` holds the first, ad hoc version of the addendum-A analysis. Its RISC-V atomic
count line is wrong: the disassembler was run without the A extension and printed zeros. Addendum A
replaces it.

## Results

**Verdict: QUALIFIED.** Implementation `b7263d21`; documentation `bf06be4c`, checked by addendum B.

| step | result | where |
|---|---|---|
| hosted | fmt; serial lib, 6,715 passed; `--tests`, 7,112 passed; the grader CLI corpus; 19 focused subsets, each with a non-zero count (`apsched` matched none); contract gate; doc enforcement; witness feature checked on 3 targets | `qual/H*`, `G*`, `X*` |
| lock order | 30 descending-pair types on C and on `dce48596`; empty diff | `qual/H5_*` |
| sizes | `.text` x86_64 +11,328, AArch64 −48,924, RISC-V +10,124. Data +8,192 on each port (`CpuCell` +128 B × 64, from the deferred-diagnostic buffer; the lock stays 64 B) | `qual/Z1_*`, `Z2_sizes.txt`, `addA/` |
| W1 / W2 / W3 | SMP2 extended SCHED-LOCK1 witness on x86_64, AArch64 and RISC-V: each `SL1W_SEAL … result=pass`, 0 gaps, max 1 ahead, both probes (1L[0] and 1G) proved, remote enqueues and `idle_wakes=3_ipi` | `qual/W1`, `W2`, `W3` |
| NC1 / NC2 | masking after the draw; a barging try-lock: each fails its hosted test | `qual/NC1_*`, `NC2_*` |
| NC3a / NC3b | a barging draw: 6 of 9 hosted `fair1_` tests fail; live x86_64, the workload passes but the grader fails it with `fair-coverage` | `qual/NC3*` |
| NC4a / NC4b | W1's log mutated (CRC recomputed): a re-acquire overtaking the peer; a service gap. Both `fair-order` | `qual/NC4*` |
| addendum A | read-only analysis of the saved kernels | `qual/addA/` |
| addendum B | the docs commit changes only the two documents; gates, fmt and the serial lib (6,715) pass | `qual/addB/` |

`dev/fair1_inventory*.txt` is the 1L/1G critical-section census (`scripts/fair1_inventory.py`) on
the base and on the candidate. `scripts/fair1_docs.py` applied the documentation edits.
