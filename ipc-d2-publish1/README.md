# IPC-D2-PUBLISH1 evidence (HARD-STOP: a rule decision is needed)

**Result.** The pass ended with a decision brief, not a repair:
- **Baseline:** dev at `4b7b71e067c0d0cf412ace3522e31e6f79f118c6`.
- **Brief:** commit `9515a943` on `wip/ipc-d2-publish1` adds it to `doc/KERNEL_UNLOCKING.md`
  (*IPC-D2-PUBLISH1*), and `BRIEF.md` here is the same text.
- **Unchanged:** no kernel, gate, grader or rule text. Dev was not moved.

| path | what |
|---|---|
| `refs_before.txt` | `git ls-remote origin` at the start of the pass |
| `analysis/inputs.sha256` | the six-event boot logs this pass read, by hash. They live on VFS-CORRECT1C's evidence commit `35267905` under `vfs-correct1c/evidence/qual-3/R_race_smp2_<port>/boot.log.gz`, and the hashes match that commit's `MANIFEST.sha256` |
| `tools/race_events.py` | attributes every `D2_PUBLISH_RACE_UNWIND` in a normalized log: block, enqueue, pair, unwind, settle, delivery, duplicate bindings, reply, CPUs |
| `analysis/race_events.jsonl` | its output for the three logs (six events) |
| `tools/check_publish_race.py` | PROTOTYPE of the proposed replacement rule (B, S1–S7 in the brief). Not wired into any gate |
| `analysis/check_publish_race.txt` | the prototype on the six events: ok |
| `analysis/check_publish_race_selftest.txt` | its controls: 11 mutants per log, each rejected by the rule it targets |
| `analysis/parks_per_cpu.txt` | split blocking-receive parks per CPU. In the default SMP2 core smokes (VFS-CORRECT1C run 3) all are on CPU 0; the SMP2 fd witness parks 7 on CPU 1 per port |
| `hosted/race_tests_base.log`, `race_tests_list.txt` | the 34 hosted tests that cover this transition, on `4b7b71e0`: pass |
| `hosted/tests_after_brief.log` | `cargo test --tests` with the brief applied: 43 binaries, 7,188 passed |

To reproduce from a checkout:

```
git show 35267905:vfs-correct1c/evidence/qual-3/R_race_smp2_x86_64/boot.log.gz | gunzip | tr '\r' '\n' > x86.log
python3 -I tools/check_publish_race.py x86.log
python3 -I tools/check_publish_race.py --self-test x86.log
```

`MANIFEST.sha256` lists every file here except itself; verify it with `sha256sum -c MANIFEST.sha256`.
