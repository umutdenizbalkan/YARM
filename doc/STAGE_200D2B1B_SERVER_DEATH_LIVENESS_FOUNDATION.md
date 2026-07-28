<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1B — Server-Death Liveness Foundation

The causal foundation under the ServerDies scenario: nine per-instance transition counters
at the real production operations, fifteen hard-fail literals at real failure sites,
twenty-four deterministic hosted races, four executable fail-closed rejections, and nine
wiring guards that prove the accounting is attached to production code rather than to
declarations.

This is a FOUNDATION stage. It contains no QEMU work, no architecture return contracts, no
feature-forwarding runners, and no live cells. `claude/stage-200d-consolidated` and `main`
are untouched.

Part 1 (the nine counters) and Part 2 (the fifteen literals) landed in `35a98f2`
(200D-2B1B-i). This document covers 200D-2B1B-ii — the races, rejections, wiring guards and
feature-boundary validation — and emits the foundation seal for the whole of 200D-2B1B.

## 1. The nine transition classes

| # | Class | Production site | Counted when |
|---|-------|-----------------|--------------|
| 0 | `LinkCreated` | `register_server_reply_link` | the `None` arm really installs a new link |
| 1 | `LinkDetached` | `take_server_reply_link` | a link was really removed (exact on `{tid, asid}`) |
| 2 | `DeferredReserved` | `server_death_work_reserve` | the reservation really succeeded |
| 3 | `DeferredPublished` | `server_death_work_publish` | after the slot write; the duplicate arm returns early |
| 4 | `DeferredConsumed` | `server_death_work_drain_next` | an item was really taken |
| 5 | `PeerDeathWinner` | `complete_server_death_over` | after `rt_commit_reply_terminal` |
| 6 | `ResultPublication` | `complete_server_death_over` | after `rt_commit_receiver_runnable` returned `Some` |
| 7 | `RunnableTransition` | `complete_server_death_over` | same site; the `else` arm counts neither |
| 8 | `CallerEnqueue` | `complete_server_death_over` | after the real `d.rtd_enqueue` |

A monotonic `SEQ` stamps every increment, so ordering is read off the two real operations
rather than assumed from declaration order.

## 2. Four executable fail-closed rejections

Each drives a real production entry point with an incarnation that never existed and
asserts that nothing was detached, claimed, published or woken.

| Case | Rejected input | Verdict |
|------|----------------|---------|
| `r01` | a deferred item whose reply-record generation no longer matches the live slot | consumed; `LinkDetached=0`, no winner, no publication, the live link survives |
| `r02` | caller TID reuse with a DIFFERENT ASID (task 1 rebound into another live address space) | `CleanupNoWake`; `ResultPublication=0`, `RunnableTransition=0`, `CallerEnqueue=0` |
| `r03` | server TID reuse with a DIFFERENT ASID presented to `take_server_reply_link` | detaches nothing; `LinkDetached` unchanged; the real link survives |
| `r04` | a second terminal identity over the SAME record slot with a different deadline token generation | rejected by the collector's four-field compare; `IPC_SERVER_DEATH_WRONG_TIMEOUT_GENERATION` |

## 3. Twenty-four deterministic races

`x01` … `x24`, no sleeps, no wall clock, no host-thread scheduling. Every one of the
twenty-four carries the same four-part obligation, enforced by a shared `assert_race`
helper:

1. the **exact nine-counter vector**;
2. the **ordering stamps** — a class that fired is stamped, a class that did not is not,
   and every causal edge whose two endpoints each fired exactly once holds strictly;
3. the **single committed terminal winner** (or none);
4. a **rejected late/stale operation** — a late terminal claim presenting a stale record
   generation and a stale server incarnation presented to `take_server_reply_link`, both
   refused by the production seams, advancing no counter and leaving the winner unchanged.

| # | Race | Vector | Winner |
|---|------|--------|--------|
| 1 | PeerDeath wins normally | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 2 | result stamped strictly before the enqueue | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 3 | Reply wins before the death drain | `1,1,1,1,1,0,0,0,0` | Reply |
| 4 | Timeout wins before the death drain | `1,1,1,1,1,0,0,0,0` | Timeout |
| 5 | late Reply after PeerDeath loses | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 6 | late Timeout after PeerDeath loses | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 7 | duplicate drain is inert | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 8 | duplicate deferred publication collapses | `1,1,2,1,0,0,0,0,0` | none |
| 9 | capacity refusal BEFORE the irreversible detach | `1,0,N,0,0,0,0,0,0` | none |
| 10 | stale reply-record generation claims nothing | `1,0,1,1,1,0,0,0,0` | none |
| 11 | stale server incarnation claims nothing | `1,0,1,1,1,0,0,0,0` | none |
| 12 | link→deferred handoff is exact | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 13 | reserve strictly precedes detach | `1,1,1,1,0,0,0,0,0` | none |
| 14 | consumption follows publication | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 15 | link creation precedes detachment | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 16 | exactly one caller wake | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 17 | exactly one terminal winner across a repeated attempt | `1,1,2,1,1,1,1,1,1` | PeerDeath |
| 18 | no reverse-link leak | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 19 | no deferred-item leak | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 20 | a losing reply leaves no orphans | `1,1,1,1,1,0,0,0,0` | Reply |
| 21 | a losing timeout leaves no orphans | `1,1,1,1,1,0,0,0,0` | Timeout |
| 22 | no user memory copy on death | `1,1,1,1,1,1,1,1,1` | PeerDeath |
| 23 | two instances do not cross-contaminate | `1,1,1,1,1,1,1,1,1` ×2 | PeerDeath |
| 24 | collector release requires userspace validation | `1,1,1,1,1,1,1,1,1` | PeerDeath |

`N` in case 9 is the queue capacity, read from the counter after the fill rather than
hard-coded, so the assertion stays exact if the capacity changes.

A class stores its LATEST stamp, so `assert_race` applies a causal edge only when both
endpoints fired exactly once. The two cases with a repeated class (8 and 17) assert the
ordering that does hold with an explicit stamp comparison of their own.

The counters, the deferred queue and the collector gate are process-global. Every case that
mutates them holds a poison-tolerant serialization guard, so the default parallel
`cargo test` run cannot interleave two instances into a vector neither case produced. The
2B1B-i counter cases take the same guard.

## 4. Nine wiring guards

| Guard | Proves |
|-------|--------|
| `w01` | EVERY production caller of the post-lock death drain invokes it after its broad guard dropped — the shared x86_64/AArch64 post-`with_cpu` section and the RISC-V Phase 3, exactly one call site each |
| `w02` | the post-lock markers live inside the drain and state `broad_lock=0`; the in-lock exit-phase markers make no post-lock claim |
| `w03` | reserve precedes the detach, capture follows the real detach, publication is attested last |
| `w04` | `TERMINAL_CLAIM` follows the real CAS, `COMPLETION_COMMITTED` follows the real publication, `CALLER_ENQUEUED` follows the real enqueue, and the commit precedes the enqueue |
| `w05` | the stale timeout token is recorded at the real arm site, immediately after the TCB publication, from `handle.identity()` |
| `w06` | no private per-port ServerDies table and no oracle-only completion helper; the oracle calls no death helper (checked against comment-stripped source, so the oracle's prose about what it deliberately does not call cannot satisfy the guard) |
| `w07` | `audit_success_path()` has a real production caller, so its literals survive linking, and nothing downstream branches on its verdict |
| `w08` | the collector release runs through USERSPACE validation: the predicate requires the caller's own marker plus the canonical code, the gate performs no production action, and the collector really consults the gate |
| `w09` | all FIFTEEN literals attach to a real production operation — each named with its enclosing production function AND an operation that function actually performs |

## 5. The fifteen literals and their production attachment

| Literal | Enclosing production function | Real operation |
|---------|-------------------------------|----------------|
| `EXIT_RETURNED` | oracle `server_run` | `exit_current_task()` |
| `DUPLICATE_DEFERRED` | `exit_task` | `server_death_work_publish(` |
| `WRONG_SERVER_IDENTITY` | `drain_server_death_post_work` | `server_death_work_drain_next(` |
| `WRONG_RECORD_GENERATION` | `drain_server_death_post_work` | `server_death_work_drain_next(` |
| `WRONG_CALLER_IDENTITY` | `complete_server_death_over` | `rt_is_blocked_receiver_exact(` |
| `WRONG_ENDPOINT_GENERATION` | `complete_server_death_over` | `rt_endpoint_generation_read(` |
| `WRONG_TIMEOUT_GENERATION` | `collect_due_reply_timeout_work` | `server_dies_stale_token()` |
| `DUPLICATE_COMPLETION` | `server_dies_counters::record` | `fetch_add(1` |
| `DUPLICATE_WAKE` | `server_dies_counters::record` | `fetch_add(1` |
| `LINK_LEAK` | `audit_success_path` | `count(T::LinkCreated) != count(T::LinkDetached)` |
| `RECORD_LEAK` | `audit_success_path` | `count(T::LinkDetached) != count(T::PeerDeathWinner)` |
| `DEFERRED_LEAK` | `audit_success_path` | `count(T::DeferredPublished) != count(T::DeferredConsumed)` |
| `TIMEOUT_WON` | `drain_reply_timeout_post_work` | `complete_reply_timeout_over(` |
| `LATE_REPLY_ACCEPTED` | `reserve_reply_win_before_copy` | `self.try_reserve_reply_win_before_copy(reply_cap)` |
| `STALE_AUTHORITY_RESTORED` | `restore_deadline_reply_lease` | `t.restore_reply_lease(owner)` |

Fourteen of the fifteen are kernel verdicts; `w09` additionally asserts userspace emits none
of them. The exception is `EXIT_RETURNED`, which reports the oracle server's own failure to
be destroyed by NR16 and therefore belongs in userspace.

## 6. Feature-boundary validation

| Check | Result |
|-------|--------|
| `cargo fmt --all --check`, `git diff --check` | clean |
| hosted, default features | 3687 passed, 1 failed (the known Stage 190B failure, unchanged), 2 ignored |
| hosted, `ipc-reply-timeout-oracle-core` | 3844 passed, 1 failed (same known failure), 2 ignored |
| `cargo build --workspace` | ok |
| feature-off kernels: x86_64, AArch64, RISC-V | build |
| oracle-on kernels: `x86-`/`aarch64-`/`riscv64-ipc-reply-timeout-oracle` | build |
| freestanding `init_server`: x86_64, AArch64, RISC-V | build |

The known Stage 190B failure is `stage190b_controlled_workload::repeated_dispatch_places_next_task_audited_one_at_a_time`
("the scheduler loop must place the next workload task by index, one at a time"), identical
at `6b5ace8`, `5488d8e`, `b620385` and `35a98f2`.

### Binary oracle-literal audit — all three feature-off architectures

For each feature-off `kernel_boot`, fourteen oracle-gated literals must be ABSENT and the
fourteen production server-death literals must be PRESENT:

```text
x86_64 : 0/14 oracle literals, 14/14 production literals
aarch64: 0/14 oracle literals, 14/14 production literals
riscv64: 0/14 oracle literals, 14/14 production literals
```

The audit is discriminating rather than vacuous: the same scan over the oracle-ON kernel of
each architecture finds 17 oracle-literal occurrences.

## 7. Foundation seal

```text
STAGE_200D2B1B_SERVER_DEATH_LIVENESS_FOUNDATION_SEAL
counters=9
literals=15
rejections=4
races=24
wiring_guards=9
counter_tests=13
arches=3
feature_off_kernels=3
oracle_on_kernels=3
init_server_targets=3
feature_off_oracle_literals=0
feature_off_production_literals=14
declaration_only_guards=0
simulated_transitions=0
qemu_boots=0
live_cells=0
result=ok
```

`qemu_boots=0` and `live_cells=0` are part of the seal, not omissions: this stage is scoped
to the hosted foundation, and no live retirement is claimed by it.
