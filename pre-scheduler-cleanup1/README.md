# PRE-SCHEDULER-CLEANUP1 evidence

Stage: SR-PLACE1 (reserve the shared-region receiver's home slot before delivery), PRINTK-FOOTPRINT1
(ring slots in `.bss`, 64-record default, 1024 for witnesses), SCHED-LOCK1-ORDER1, SCHED-SAT-EVIDENCE1,
ARTIFACT-CARGO-BIND1. Baseline B = `2866be83`. Frozen candidate F = `da7bb379` on
`wip/pre-scheduler-cleanup`; record commit `93edaac9` (docs only).
Verdict: **NOT QUALIFIED as delivered**. Every check passed on all three ports except the RISC-V
sched-time witness, whose saturation-at-deadline premise was missed in both qualification runs. The
guards failed closed and completions were conserved. `claude/yarm-kernel-unlock-u0-f2zh8p` was not
fast-forwarded. The record is in `doc/KERNEL_UNLOCKING.md` ("PRE-SCHEDULER-CLEANUP1"); follow-ups are
in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`, and scheduler design inputs in `doc/SCHEDULING_CONTRACT.md` §19.

| path | contents |
|---|---|
| `evidence/qual/` | the frozen matrix at F vs B: `summary.txt` (every step, rc, seconds), every step log and run directory, `predeclared.sha` (runner and helpers at the freeze, then Addenda A and B), `notes.txt` (classification of every non-zero step), `bindings.txt`, `X_footprint.txt`, H2b clippy classes, H6 lock-order attribution, `W1b_*` (Addendum A, shared-region with `OFFSET_SELECTION=1`), `W3b_riscv64` (Addendum B), `R_*` (contract gates on the record tree) |
| `evidence/sr/` | SR-PLACE1 development: baseline reproducer patch and log (r1–r3 fail on B), negative controls srnc1–srnc6 (patch and log each), development hosted runs |
| `evidence/fp/` | footprint development: B and candidate builds, ring sizes 128/64/32 (default and strict core smokes), the `.text` attribution of SCHED-TIME-FOUNDATION1's growth |
| `evidence/sl/` | SCHED-LOCK1-ORDER1: the scratch boundary delay (never committed; v1 and v2 were ineffective, v3 reached the boundary), six runs each on B and F with `analysis.txt` |
| `evidence/sat/` | SCHED-SAT-EVIDENCE1 development: live iterations ST-* / FT-* (not qualification), hosted runs (`.superseded` = replaced by a later revision) |
| `scripts/` | the matrix runner, Addenda A and B, footprint, lock-order attribution, clippy comparison, ring measurement, controls and run scripts, the doc drafts, the marker scan, and the ls-remote snapshot taken before any push |

Files over 1 MB are gzipped, and build outputs (ELF, binary images, cpio) are excluded; their hashes are
in each run's `sha256.txt` and `artifact-binding.txt`. `MANIFEST.sha256` covers every file
(`sha256sum -c MANIFEST.sha256`). Logs are kept exactly as recorded, and none was repaired.
