# SCHED-LOCK2B evidence

Candidate `79e898e9` (implementation, census guard, regressions) and `dce48596` (contract §21 and
the stage record, docs only), both on `wip/sched-lock2b`. Baseline: `c9fbc7ef`.

* `scripts/sl2b_qual.sh` is the focused qualification runner, and `scripts/sl2b_addA.sh` is
  addendum A. Both were hashed in `scripts/predeclared.sha` before they ran. `refs_before_sl2b.txt`
  records the remote refs at the start.
* `evidence/qual/summary.txt` gives every step's rc, duration and tree porcelain:
  * H1–H4: fmt, the serial lib suite, integration, and 23 focused subsets.
  * H5: lock-order pair types. C and B have the same 30 types (`H5_pairtypes.diff` is empty).
  * G1–G2: the contract and doc gates.
  * B1–B3: default builds.
  * L1–L5: the live cells:
    * x86_64 functional single-CPU direct request/reply;
    * AArch64 direct request/reply and the AArch64 exit-current-task witness;
    * RISC-V direct request/reply and the RISC-V exit-current-task witness.
  * Z: the home-refusal scan over text logs.
  * NC1–NC2: negative controls. Each is expected to fail, and the tree is restored afterwards.
* `evidence/qual/D1_doc_commit_lib.log` is the serial lib suite over the tree with the docs
  commit, since some tests read `KERNEL_UNLOCKING.md`.
* `evidence/addA/` holds the same three direct smokes on the baseline. They print the identical
  `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL … nr7_ok=0 … result=fail` diagnostic.
* `evidence/dev/` holds the two development lib runs. They are kept, with their failures, as
  found: 23, then 6, all source guards and new tests that were then corrected.

Kernel images copied into log directories by the smokes (`*.bin`) are omitted. Logs over 1 MB are
gzipped. `MANIFEST.sha256` covers every file.

Verdict: QUALIFIED. See `doc/KERNEL_UNLOCKING.md` ("SCHED-LOCK2B") and
`doc/SCHEDULING_CONTRACT.md` §21.
