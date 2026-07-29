<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D4 — x86_64 ServerDies live retry (NOT sealed)

The runner was executed **unchanged** at `f64ce7a`. **No live cell is claimed.** Stage
200D-2B1D3's terminal arming is confirmed live and the kernel-side chain now completes end to
end; two defects remain, neither of which was fixed here.

## The kernel chain is complete and correct

```text
IPC_SERVER_DEATH_TIMEOUT_ARMED                       terminal + deadline armed (2B1D3)
IPC_SERVER_DEATH_REQUEST_RECEIVED / REPLY_CAP_RECEIVED
IPC_SERVER_DEATH_EXIT_ENTERED     nr=16 role=server  tid=10008
IPC_SERVER_DEATH_DEFERRED_RESERVED
IPC_SERVER_DEATH_LINK_CAPTURED    record_index=1 record_generation=17 detached=1   (2B1D1)
IPC_SERVER_DEATH_DEFERRED_PUBLISHED
IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN                broad_lock=0
IPC_SERVER_DEATH_TERMINAL_CLAIM   terminal=PeerDeath result=won caller_tid=1 caller_asid=1
IPC_SERVER_DEATH_COMPLETION_COMMITTED code=10 runnable=1 broad_lock=0 result=ok
IPC_SERVER_DEATH_CALLER_ENQUEUED  enqueues=1 broad_lock=0 result=ok
IPC_SERVER_DEATH_OK  terminal=PeerDeath death_result=ServerDied caller_wakes=1
                     reply_aliases_invalid=1 reply_copies=0 result=ok
IPC_SERVER_DEATH_DRAIN outcome=Woken
```

The runner's own counters agree: **`caller_wakes=1 peer_death_winners=1`**. Six of the eight
sealing requirements are met — terminal arm, reverse-link capture, deferred drain, PeerDeath
winner, `ServerDied=10` publication, and exactly one caller enqueue/wake.

## Defect 1 (chain-blocking) — a woken task is never dispatched

`IPC_SERVER_DEATH_USER_VALIDATED` never appears, because the caller never runs again.

The caller (tid 1) is committed `runnable=1` and enqueued (`enqueues=1`). After that point the
log contains **zero** dispatches, and this repeats 220 times until the boot times out:

```text
D6_LOCAL_DISPATCH_STEP_SPLIT cpu=0 tid=None runnable=2
D6_LOCAL_DISPATCH_SEAM_COUNT cpu=0 n=129 tid=None
SCHED_ENTER_IDLE_HLT cpu=0
```

The local dispatch seam **sees two runnable tasks and selects none**, then halts. So the
server-death mechanism did its whole job — the caller is runnable with the canonical
`ServerDied = 10` in its saved context — and the scheduler will not pick it up.

This is newly *reachable* rather than newly written: before this stage's lineage nothing ever
woke a blocked caller by this route, so the path from "death wake" to "dispatch" had never been
exercised live. It is a scheduler/dispatch defect, not a server-death one, and diagnosing
`runnable=2 → tid=None` in the D6 split-dispatch seam is a stage of its own.

## Defect 2 (accounting) — `LINK_LEAK created=54 detached=1`

```text
IPC_SERVER_DEATH_LINK_LEAK       created=54 detached=1 result=fail
IPC_SERVER_DEATH_TRANSITION_COUNT class=links_created count=54 expected=1 result=fail
IPC_SERVER_DEATH_TRANSITION_AUDIT vector=[54, 1, 1, 1, 1, 1, 1, 1, 1] result=fail
```

This is **not** a resource leak. The ordinary reply path does close its links —
`ipc_reply` calls `finalize_server_reply_link_for_record`. The mismatch is in what the
counters count:

* `LinkCreated` is recorded by `register_server_reply_link`, which since Stage 200D-2B1D1 runs
  for **every** bound ordinary `IpcCall` in the system — 54 of them across this boot.
* `LinkDetached` is recorded only by `take_server_reply_link`, the **exit** path. Normal reply
  completions close their link through `unregister_server_reply_link`, which records nothing.

So `audit_success_path()` compares a system-wide creation count against a single death's
detach count, and additionally expects every class to equal exactly 1. That expectation was
only ever true when the direct transaction was the sole link creator and the counters were
reset per hosted case. `reset_instance()` has no live caller, so in a real boot the counters
accumulate from boot.

Fixing it is a design decision, not a one-line wiring change — either scope the counters to the
death being audited, or count both close sites and drop the "== 1" expectation. Both change
what the Stage 200D-2B1B foundation's nine-counter vector means, so it belongs in its own
stage rather than being decided here.

Note this makes the vector `[54, 1, 1, 1, 1, 1, 1, 1, 1]` self-consistent: every class **except**
`LinkCreated` is exactly 1, which is the single-death evidence the audit was built to show.

## What was NOT done

Neither defect was fixed. The stage authorised fixing *one* newly exposed production wiring
defect; Defect 1 is a scheduler-selection bug and Defect 2 is an accounting-model decision, and
fixing either alone would not have sealed the cell. Nothing was weakened: the marker chain and
its ordering, the forbidden set, the identity checks, the wake and PeerDeath counts and the
binary audits are exactly as prepared, and the oracle was not redirected to another IPC path.
RUN_A passed: `oracle_literals_present=0`, `production_literals_missing=0`.

## Progress against the sealing requirements

| Requirement | Status |
|---|---|
| terminal arm | ✅ live (new in 200D-2B1D3) |
| reverse-link capture | ✅ live |
| deferred drain | ✅ live |
| PeerDeath winner | ✅ live, exactly one |
| `ServerDied=10` publication | ✅ live, `code=10 runnable=1` |
| exactly one caller enqueue/wake | ✅ live, `enqueues=1` |
| userspace validation | ❌ blocked by Defect 1 |
| late timeout scanned and rejected | ❌ not reached — the scan runs after the caller validates |

## Status

```text
STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL arch=x86_64 result=fail
```

No seal is emitted by this stage.
