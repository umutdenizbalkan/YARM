<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM recovery sources — what was inspected, what was missing

Inventory of the evidence behind `doc/YARM_MASTER_CONTEXT_SEED.md`,
`doc/YARM_DEVELOPMENT_HISTORY.md`, `doc/YARM_KERNEL_UNLOCK_HISTORY.md` and
`doc/YARM_BUGS_AND_LESSONS.md`. Compiled 2026-11-05, read-only: no source file, branch, ref
or transcript was modified, and nothing was committed or pushed.

## Evidence labels used in all five files

| Label | Meaning |
|---|---|
| **[VC] VERIFIED CURRENT** | confirmed by reading source at `b81e0317` (the current `main`) |
| **[VH] VERIFIED HISTORICAL** | confirmed by a Git commit, a committed document at a named commit, or a retained test log |
| **[SR] SESSION-REPORTED** | stated in an AI session (transcript, compaction summary or PR body), not independently re-checked here |
| **[INF] INFERRED** | reconstructed from incomplete evidence; the reasoning is given |
| **[UNK] UNKNOWN** | cannot be established from available evidence |

## 1. Claude Code session records

| Source | Available | Notes |
|---|---|---|
| `~/.claude/projects/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043.jsonl` | **yes** (184 MB, 60 188 lines) | The only transcript on this machine. First record is a compaction summary dated **2026-10-09T18:10Z**; last record 2026-11-05. Dialogue text from before 2026-10-09 is **not** preserved — only its summary. Contains 30 compaction summaries, 133 user messages, 23 full stage briefs. |
| `…/e3c47cf0…/tool-results/*.txt` | yes (7 files) | Large tool outputs from this session only. |
| `~/.claude/tasks/e3c47cf0…/*.json` | yes | Task list #1–#109 of this session (Oct–Nov stage breakdowns). |
| `~/.claude/history.jsonl` | **no** | Does not exist. |
| Subagent transcripts | **no** | None exist. |
| `~/.claude/backups/` | yes | `.claude.json` configuration backups only; no conversation content. |
| Earlier Claude sessions (19, May 11 – Aug 10) | **no transcripts** | Known only by `Claude-Session:` trailers in commits and by PR bodies (§3). |

Derived working files (scratch, not committed):
`/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/recovery/` —
`briefs_full.txt` (all 23 briefs verbatim), `summaries_full.txt` (all 30 summaries verbatim),
`summary_errors.txt`, `transcript_users.txt`, `stage_index.txt`, `main-all.txt`,
`main-firstparent.txt`, `sessions.txt`, `branches.txt`, `CHECKPOINT.md`, `FINDINGS.md`.
These live in a session scratchpad and may not survive; everything they established is
restated with provenance in the five documents.

### Briefs recovered verbatim (owner instructions, 2026-10-12 → 2026-11-05)

SHARED-REGION-OFFSET1 (10-12), SHARED-REGION-LIFETIME1 (10-12), SHARED-REGION-LIFETIME2 (10-12),
BROAD-LOCK-RETIREMENT1 (10-13), VM-INVALIDATION1 (10-13), AP-SCHED1 (10-13), AP-SCHED2 (10-14),
AP-SCHED3 (10-19), AP-SCHED4 (10-20), VM-INVALIDATION2 (10-21, plus a "continue" note),
VM-INVALIDATION2-ACCEPTANCE (10-21), VM-INVALIDATION2-SEAL (10-21), SCHED-CONTRACT1 (10-21),
SCHED-CONTRACT1-CORRECTION (10-26), SCHED-OVERFLOW1 + KERNEL-NOFP1 (10-26),
KERNEL-NOFP1-ACCEPTANCE (10-26), BUILD-GATES1 (10-26), SCHED-LOCK1 (10-27),
SCHED-LOCK1-ACCEPTANCE (10-28), SCHED-LOCK1-SEAL (10-28), SMP3-EVIDENCE1 (10-28),
KTHREAD1 (11-02), KTHREAD1-ACCEPTANCE (11-03), KTHREAD-CLOSURE1 (11-03), and this recovery
request (11-05). The BL1–BL6 backlog brief (before 10-09) survives only inside compaction
summary #0.

## 2. Other AI histories

| Source | Available | Notes |
|---|---|---|
| Codex CLI local history (`~/.codex`, `~/.config/codex`) | **no** | Absent. Codex's work is visible only as 43 merged `codex/*` PR branches (Feb–May) and their commits. |
| **ChatGPT conversation "YARM kernel unlock"** (`6a7a20ea-c638-83eb-8679-0715c202753b`) | **no** | No export, `conversations.json` or transcript exists anywhere on the filesystem (searched `/` for `conversations.json`, `*chatgpt*`, `*6a7a20ea*`). The URL is not readable from here. **None of its content is reproduced; nothing attributed to it is claimed.** [INF] The owner-supplied canonical stage definitions 199C–205D in `doc/KERNEL_UNLOCKING.md` §0 ("owner-supplied and authoritative … not derived from branch history") and the stage briefs are the most likely written residue of that planning, but this cannot be verified. |
| Repository agent instructions | yes | `doc/AGENTS.md` (pointer), `doc/AI_AGENT_RULES.md` (1 121 lines, canonical rules), `doc/KERNEL_TEST_RULES.md`, `doc/DOCUMENTATION_MAP.md`. No root `CLAUDE.md` exists. No earlier context-seed file exists in the tree (the doc guard forbids `*_NEXT_CONTEXT.md`). |

## 3. Git and GitHub

| Source | Coverage |
|---|---|
| Local checkout `/home/user/YARM` | **shallow** (boundaries `70561d03` 2026-09-30 and `f782227a` 2026-09-28); its HEAD is local branch `wip/kthread-closure1-a` at `136e8ae2`, **not** current `main`. Not modified. |
| Full bare clone (scratch, `…/scratchpad/recovery/full.git`) | complete: `main` = **3 186 commits**, `3088f528` (2026-02-25 "Initialize repository") → `b81e0317` (2026-11-04); **146 branches** (143 `wip/*`, 2 `claude/*`, `main`); **0 tags**; 459 merge commits on `main`. |
| Authorship | owner account 1 952 (Feb–Aug; includes Codex-driven PRs), Claude 1 466 (May–Nov). |
| Claude sessions named in commit trailers | 20: `session_014WLNWKjVWZ6JvX1hEnrPsS`, `01BsXurWeuhuakvAS7L5PaBr`, `01Kt7txpx2PQxa8oSUoqwAUf`, `01LW7PhWxjAhVury4vZuUKeY`, `01QGashuQXjPS8MBXUErb8mq`, `01QzqYunFU1U93obfRMirtPi`, `01LjU5fSH5KeeZ6wud3eC98e`, `01Enxs72tE5RKKKRbzChyUSa`, `01EcjR3AutU1KAV98uQacNwx`, `017v7qCZZ2rBjNqoqZchoHDA`, `01CLXcBXQECVp11gdnrTGGvm`, `01FWUZxAKTrQy1RRqvV3uhCV`, `018MSFCSwBnid4TSNZ3LaUfN`, `012UHwjF3PKFnAnfqrpqb9jU`, `01J2AGHeskTBYUApxwtWdQRc`, `01KdLQBoMMbNg9hh1od1QXRg`, `015kNpLCVorth4VzGvFPDWo2`, `01Kx4TRP41kxwpuLDama1c1E`, `011ftPQ2qW7RmE5nTwzYZBk9` (2026-05-11 … 08-10) and the current `01QLpmtT4bLN2gX3DCKAXYp8` (2026-08-11 … 11-04, 804 commits). |
| GitHub issues | **0** (none exist). |
| GitHub pull requests | **542** (#1 2026-02-25 `codex/implement-vm-primitives-in-microkernel` … #542 2026-08-10 `claude/kernel-unlock-audit-i7nt1t`); 440 merged into `main`. No PR after 2026-08-10 (later deliveries were direct fast-forwards; briefs forbade PRs). PR bodies carry session reports; only #1–#5, #540–#542 were sampled here. |
| Work after the reported checkpoint | **none**: no branch has commits newer than `b81e0317`. Unmerged `wip/*` heads are either September record-only U9 commits or duplicates whose content landed on `main` under other SHAs (e.g. `wip/riscv-illegal-instruction` → `d38f641d`…`d43940f2`; `wip/reply-wins-witness` → `30c9bf4e`…`288e8339`). |

## 4. Committed documents used (all at `b81e0317` unless stated)

| Document | Last edited | Role |
|---|---|---|
| `doc/PROJECT_HISTORY.md` | `28debeab` 2026-08-04 | milestone log Phase 0 → Stage 199D and the accepted/refused seal ledger |
| `doc/KERNEL_UNLOCKING.md` (33 747 lines) | `b81e0317` | canonical stage ladder §0, invariants §3, every stage record from Stage 101 to KTHREAD-CLOSURE1 |
| `doc/STATUS.md` | `92713255` 2026-10-13 | live-status page — **partly stale** (see §6) |
| `doc/SCHEDULING_CONTRACT.md` | `b81e0317` | normative scheduling contract, SC-* requirement ids, §18 kernel threads |
| `doc/AI_AGENT_RULES.md`, `doc/AGENTS.md` | — | existing agent rules (partly stale, see §6) |
| `doc/KERNEL_UNLOCK_AUDIT.md`, `doc/KERNEL_LOCKING.md`, `doc/IPC.md`, `doc/DOCUMENTATION_MAP.md` | — | consulted by heading/section, not read in full |

## 5. Retained test artifacts

Scratch qualification logs for the KTHREAD-CLOSURE1 stages
(`…/scratchpad/kc1/qa1..qa4`, `qb1`, `qc1`, `qc2`, `qd1`) with `summary.txt` per run. Older
qualification logs exist only as numbers quoted in `doc/KERNEL_UNLOCKING.md` records.

## 6. Contradictions and stale sources found

1. **`doc/STATUS.md` §1 and §5 are stale.** They say the production scheduler is BSP-only, that
   RISC-V SMP scheduling is off and that AArch64 SMP is deferred; AP-SCHED1–4 (2026-10-13 → 10-21,
   `e9c8173c`, `7a76f789`, `8cb30dd1`, `df5ee7b6`) made production user scheduling run on
   secondary CPUs on all three ports. Its "U9 remains OPEN" paragraph predates the U9 closure
   (`93900ae0`, 2026-09-24).
2. **`doc/KERNEL_UNLOCKING.md` §0.3 wording predates U9 and BLR1.** It still describes broad-lock
   callsites; the file itself warns that this wording is historical and that the U9 census
   "completes no stage other than 204A". The canonical stage table was **not re-classified**
   after the broad lock was deleted — a deliberate owner rule, but easy to misread either way.
3. **Source comment contradiction, `src/kernel/boot/mod.rs`:** the block at line ~4024 says
   AArch64 and RISC-V direct IPC "remain proof-gated"; `ipccall_direct_production_enabled()`
   (line 4154) returns true on all three architectures. The code is authoritative [VC].
4. **`doc/AI_AGENT_RULES.md`:** §3.4 says syscall 27 must not be removed yet — it was removed in
   Stage 197A (`696db6cb`, 2026-07-13); §6 lists "change x86_64 SMP in smoke" as prohibited
   because x86 SMP is not validated — AP-SCHED4 later qualified x86 SMP at 2/4/8 CPUs.
5. **`doc/RPI5_BRINGUP.md` §12–13 and `doc/STATUS.md` §1.4 are stale:** both say HH-5 is deferred
   because the high-half initrd/allocator bridge is not ready; the bridge, a high-half boot
   allocator, kernel-entry shim and BOOT-4 heap landed 2026-06-21/22 (`1e62310a` … `d1374f9a`).
   The hardware result after 06-22 is not recorded anywhere [UNK].
6. **VFS shared I/O, three statements that look contradictory but are not:** `doc/VFS.md` §5
   ("unsupported in production"), the source flags `VFS_READ_SHARED_REPLY_ENABLED = true` /
   `VFS_WRITE_SHARED_REQUEST_ENABLED = true`, and `doc/AI_AGENT_RULES.md` §6 ("do not enable
   `VFS_READ_SHARED_REPLY_ENABLED`"). Source comments resolve it: the flags mean the helpers and
   the PM death path are proven; the default `handle_request` still rejects the shared opcodes;
   a RAMFS-only live route exists behind separate Stage 84/85 gates. The `AI_AGENT_RULES` §6 row
   is stale as worded (the flag has been `true` since `cb0c38b4`, 2026-06-09).
7. **Draft-seed errors found in the preservation audit (2026-11-05) and corrected:** the first
   draft said VFS work and RPi5 HH-5 had "not started". Cause: `doc/KERNEL_UNLOCKING.md` §0.8
   says they were "not started **by SCHED-CONTRACT1**", and the stale RPi5/STATUS pages were
   taken at face value. Corrected with the evidence above in all five files.
8. **Session summaries vs. records:** where a compaction summary and a committed record differ,
   the committed record was preferred and the difference noted in the relevant file.

## 7. Coverage limitations

* Pre-2026-10-09 conversation text with the owner (including the original U-series, 199-series
  and QEMU-* briefs) is not available verbatim; only commits, records and PR bodies.
* The ChatGPT planning conversation is entirely unavailable.
* PR bodies were sampled, not read exhaustively (542 PRs).
* `doc/KERNEL_UNLOCKING.md` was indexed by headings and read section by section for the parts
  cited; not every one of its 33 747 lines was read.
* No build or boot was performed for this recovery; current-state claims marked [VC] were
  checked by reading source, and test counts come from the Stage D qualification log
  (`qd1`, 2026-11-04) on a tree whose code equals `b81e0317`'s.
