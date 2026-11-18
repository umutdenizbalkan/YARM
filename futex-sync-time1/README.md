# FUTEX-SYNC-TIME1 evidence

Timed mutex acquisition and absolute condition-variable waits in `yarm_user_rt::futex_sync`, on
the ABI v13 futex primitives. No kernel source change outside `cfg(test)`; ABI v13 unchanged.

- **Baseline B:** `253ca580` (the delivered FUTEX-ABS-BITSET1 head).
- **Candidate C:** `f88db55a2cd0a9d979b08a2ab5ce6b54feda0443` = `3bd3e7dd` (code, tests, witness,
  grader, ABI docs) + `f88db55a` (the runtime tests' spin counter kept out of the broad-lock
  census; behaviour unchanged).
- **Documentation D:** `c4cac895`. It touches only `doc/KERNEL_UNLOCKING.md` (the stage record),
  so its tree differs from C's in that one file.
- **Verdict: QUALIFIED** on C (run 3). The record is the FUTEX-SYNC-TIME1 stage record in
  `KERNEL_UNLOCKING.md`; the API contract is `SYSCALL_ABI.md`, *Timed mutex and
  condition-variable waits*.

## Layout

| path | what |
|---|---|
| `scripts/fst1_qual.sh` | the qualification runner, steps in order |
| `scripts/nc/nc.py` | the negative-control mutations, applied to `crates/yarm-user-rt/src/lib.rs` |
| `scripts/dev/` | the development dry-run and witness drivers (not graded) |
| `evidence/qual/predeclared.sha` | hashes of the runner, `nc.py`, both graders and the smoke script, plus C's commit and tree, written before the run |
| `evidence/qual/summary.txt` | every step: command, start, rc, duration; worktree head and porcelain at each step |
| `evidence/qual/*.log[.gz]` | one log per step (logs over 1 MB gzipped) |
| `evidence/qual/W1..W3/` | the live witness boots (x86_64, AArch64, RISC-V; SMP2, GP): build logs, boot logs, artifact binding, grade |
| `evidence/qual/NCL2/` | the live negative control (condvar timeout without reacquiring, RISC-V, full build) |
| `evidence/qual/kernels/SHA256SUMS`, `initramfs/SHA256SUMS` | C and B default kernels and core initramfs images (hashes only) |
| `evidence/qual/Z2_sizes.txt`, `S_profile_markers.txt`, `H5_*` | sizes, profile/budget markers, lock-order pair types (`H5_B_reused_from_a8d7952d.pairs.txt` is pass 6's candidate run, code-identical to B) |
| `evidence/qual-1-FAILED-H3/` | run 1 on `3bd3e7dd`: H3 failed (`broad_lock_census_guard` counted a thread-local `with` closure in `futex_sync_tests.rs` as broad callsites); stopped by hand; NOT qualified |
| `evidence/qual-2-ABORTED-restart/` | run 2 on C: cut off by a host restart during H3; no result |
| `evidence/addA/initramfs_sections.txt` | post-run analysis: on x86_64 and RISC-V, every user binary's `.text`/`.rodata`/`.data` in C's and B's core initramfs is byte-identical; the image hashes differ only through local symbol names (LLVM per-module hash) |
| `evidence/dev/` | development evidence (not graded) |

## Matrix (expected results)

All rc=0, except the controls: eight `NC_hosted_*` rc=101 and
`NC_live_nc2_timeout_without_reacquire_riscv64` rc=1.

- **Hosted:** serial lib 6,776 passed; `--tests` 43 binaries, 7,173 passed; `yarm-user-rt` 142
  (`futex_sync_tests` 29), `yarm-kernel` 10, `yarm-ipc-abi` 211; both grader self-tests; 12
  focused subsets (`sync_time1` 7, the r30 guard 1, …).
- **Lock order:** 30 descending-pair types on C and the reused baseline run, `diff_lines=0`.
- **Gates:** contract gate, doc enforcement.
- **Checks:** cross-target default kernel, witness kernel, sync witness init, time witness init.
- **Builds and smokes:** C and B builds; C kernels byte-identical to B's (and to pass 6's
  qualified kernels); GP, RT and RT+budget core smokes on C.
- **Live:** W1–W3 `FUTEX_SYNC_WITNESS_SEAL … result=ok`.
- **NCs:** hosted runtime/kernel failing tests: deadline reset 8/2, timeout without reacquire
  5/1, `CONTENDED` dropped 8/2, predicate recheck suppressed 5/1; live: C2 `timed_out held=0`.
- **Post-NC:** porcelain 0 in both worktrees.

Reused, not rerun: pass 6's kernel-level timed/bitset witness (W1–W3 of FUTEX-ABS-BITSET1), as
no kernel source changed.

## Development (`evidence/dev/`)

- `lib1.log` / `tests1.log`: early full runs. `H/`, `H2/`: the runtime and kernel suites with the
  busy-poll guard, and the census guard plus `--tests` on the fixed tree.
- `ncdry/`: dry runs of every mutation on a scratch copy. `dry1` INVALID (disk full). `dry2`: the
  first NC3 (slow path swapping `1`) spun without parking and was OOM-killed or stopped by hand.
  `dry3`: the final mutations, each failing both suites.
- `W/`: development witness boots, kept as run, each with its status:
  - `riscv64-dev1` INVALID (disk full).
  - `aarch64-dev1` INVALID (the tree changed during its build).
  - `aarch64-dev2` FAIL. The witness's parked-waiter confirmation could put the oldest waiter
    behind a later arrival; it was fixed in the cell, and no grader rule changed.
  - `x86_64-dev1` and `aarch64-dev3` STOPPED (two scripts ran at once).
  - `riscv64-dev2`, `aarch64-dev4`, `x86_64-dev2` and `riscv64-dev3` ok.
