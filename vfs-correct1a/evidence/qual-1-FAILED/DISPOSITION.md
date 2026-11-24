# Qualification run 1 — FAILED (candidate 55447e80, tree fdcc1e41)

Every step ran in order; nothing was rerun in place. Two failures, both holes in the evidence,
neither in the repair:

1. **`W_vfs_riscv64` rc=1, GRANT.** The grader required driver_manager's
   `DRIVER_MANAGER_ENTRY` after PM's `PM_SPAWN_FROM_MO_DONE image_id=7`. PM logs that line only
   after NR 29 returns; on this boot the kernel ran the child first (`boot.log`: entry at
   normalized line 11447, DONE at 11564). Every VC1A line was correct (`VC1A_END fails=0`). The
   live AArch64 control (`NCLA/`) showed the same order and was also flagged by GRANT, in
   addition to its intended BYTES failure.
2. **`NC_hosted_ncC_uncapped_backend_read` rc=0.** Removing the resident handler's 104-byte cap
   passed every test: the client never asks for more than 104 bytes, so no test reached the
   server's cap.

Fixes, both in `5bd96968`: a raw 4096-byte READ is sent to `handle_completion` directly
(`vc1a_resident_handler_caps_an_oversized_read_before_the_backend`); the GRANT rule is anchored
on the grant PM received before its NR 29 call (self-test 19 mutants, including the child-first
order). A diagnostic regrade of this run's boots with the fixed grader (not a qualification
result) gave `result=ok` on all three witness boots and, for `NCLA`, BYTES/ERRORS/INIT/CORE only.

Everything else passed: hosted (6,776 lib; 43 binaries, 7,173), crates, subsets, gates, checks,
builds (C kernels byte-identical to B), core smokes, the x86_64 and AArch64 witnesses, ncA and ncB
hosted (rc=101) and ncA live (rc=1). The runner and its predeclared hashes for this run are
`vc1a_qual-run1.sh` and `predeclared-run1.sha`.
