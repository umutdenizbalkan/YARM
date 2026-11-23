# addA — post-run artifact analyses (FUTEX-SEAL1)

- `kernel_sections.txt`: the default kernels of C (FUTEX-SEAL1) against B (c4cac895), section by
  section, extracted to FILES. `.text` and `.data` identical on all three ports; `.rodata` differs in
  18 aligned u32 words, all panic-location line numbers shifted by the lines this pass adds to
  `src/kernel/boot/futex.rs`, `src/kernel/scheduler.rs` and `src/runtime.rs`. Computed on run 2's
  kernels; run 3's C kernels are byte-identical to run 2's (sha256 `ea10316e…`, `44170087…`,
  `d539cf7a…`: only init and a grader changed between them), so it applies to run 3 unchanged.
  Section sizes (`.text`, `.rodata`, `.data`, `.bss`) are equal on all three ports.
- `p7_initramfs_sections_recheck.txt`: ERRATUM for FUTEX-SYNC-TIME1 (pass 7)
  `evidence/addA/initramfs_sections.txt`. That analysis piped `llvm-objcopy` output to
  `/dev/stdout`, which writes nothing, so it compared empty outputs and is INVALID as a method (run
  1 of this pass repeated the mistake for kernels: `qual-1-FAILED/run1_kernel_sections.INVALID.README`).
  Redone through files on the same artifacts: 253ca580 rebuilt in a temporary worktree gives
  byte-identical core initramfs images to pass 7's B (x86_64 `6d5ed25c…`, RISC-V `f3556607…`), and
  this pass's B (c4cac895, code-identical to pass 7's candidate f88db55a: the diff is doc/ only) is
  byte-identical to pass 7's C (`94efa924…`, `73ad78c1…`). Every user binary's `.text`, `.rodata`
  and `.data` (byte counts shown, none empty) are identical: pass 7's conclusion holds; its method
  did not show it.
