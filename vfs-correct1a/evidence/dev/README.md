# VFS-CORRECT1A — development runs (not graded)

| run | tree | disposition |
|---|---|---|
| `H2-pre.log` | uncommitted repair (before the witness) | root serial lib 6,776 passed |
| `w1-aarch64` | uncommitted repair + witness | `VFS_CORRECT_WITNESS_SEAL arch=aarch64 result=ok` |
| `w1-x86_64` | same | `result=ok` |
| `w1-riscv64` | same | `result=ok` |
| `ncA-aarch64` | same + `nc.py ncA_count_without_copy` (fresh build) | `result=fail` — BYTES "wrong destination bytes: differ from the fixture at offset 0", READS passing (counts right); also ERRORS (the recovery read checks bytes), INIT (bytes) and CORE (END fails=7). No hang. |

**Incident (kept).** `run_dev2.sh` restored the mutated file with `git checkout -- crates/yarm-user-rt/src/vfs_client.rs`. The repair was not yet committed, so this restored the baseline file and discarded the uncommitted client changes (the four boots above had already used the correct source). The two edits were re-applied verbatim from the session transcript (`recover_0_head.sh`, `recover_1_tests.sh`), formatted, and re-checked (yarm-user-rt 149 tests, yarm-fs-servers 578); the result is `vfs_client.rs.recovered`. The qualification runner mutates only a committed candidate, so its restores return the committed file.
