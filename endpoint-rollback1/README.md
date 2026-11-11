# ENDPOINT-ROLLBACK1 evidence

Evidence-only branch: no source code. It belongs to `wip/endpoint-rollback1`.

| Item | Commit |
|---|---|
| candidate qualified | `b9a29032` (repair + re-derived tests `f540d447`, reproducers `b9a29032`) |
| doc commit, delivered tip | `a7a27fb4` |
| baseline | `3148a754` |

Contents:

* **Schedule and runners:** `er1_schedule.md`, `er1_matrix.sh`, `er1_lockattr.sh`,
  `er1_clipcmp.sh` and `predeclared.sha`. The hash file covers them and also the patch, test and
  negative-control sources, taken before the qualifying run.
* **Patch and tests:**
  * `er1_patch.py`: the production repair;
  * `er1_testfix.py`: the re-derivation of the two existing tests;
  * `er1_tests.rs`: reproducers E1–E8, with the failure-injecting `FailingCommitOwners`.

  All three are as applied to `3148a754`.
* **Negative controls:** `er1_nc.py` and `er1_nc_run.sh`, the six frozen controls (`nc.sha`).
* **Docs:** `er1_docs.py`, the doc commit's generator, and `er1_record.md`, the stage record.
* **Raw logs:** `raw/er1-evidence-logs.tar.xz` holds every raw log, rc, summary, artifact binding
  and NC patch. Files are listed in `filelist.txt` and hashed in `raw-logs.sha256`, both relative
  to the archive root. The archive contains:
  * `qual/`: the qualifying run of `b9a29032`. It includes `H6_attribution/` (per-test lock order
    for F and B) and the clippy class comparison.
  * `qual-D/`: fmt, hosted and integration on `a7a27fb4`.
  * `nc/`: NC1–NC6 and the unmodified-candidate control.
  * `compare/`: per-profile error sets of F (C2) and B (C4).
  * `baseline_reproducers.log`: E1–E8 on `3148a754` plus only the new tests. Six fail.
  * `precommit/`: the development runs.
    * `baseline_first_attempt.log` is the first baseline run. In it E4's fill and E8's failure
      point were wrong: an entry point of zero now fails in image provisioning, before any
      endpoint exists. Both were corrected before the recorded baseline run.
* **Excluded:** kernel and rootfs binaries that the UART/PL011 witnesses write into their log
  directories. They are listed in `excluded-binaries.txt`; the artifact bindings in the logs
  carry their hashes.
