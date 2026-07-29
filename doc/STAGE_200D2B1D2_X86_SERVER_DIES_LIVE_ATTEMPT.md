<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D2 — x86_64 ServerDies live attempt (NOT sealed)

The x86_64 exact-commit runner was executed **unchanged** at `49056f2`. **No live cell is
claimed.** Stage 200D-2B1D1's reverse-link wiring is confirmed working live, and the boot
advanced to the next unwired seam.

## What changed since the previous attempt

Stage 200D-2B1D-x86 stopped at `IPC_SERVER_DEATH_DEFERRED_RESERVED` with `LINK_CAPTURED`
absent. That gap is closed — the ordinary-path registration added by 200D-2B1D1 works on real
hardware emulation:

```text
IPC_SERVER_DEATH_LINK_CAPTURED      server_tid=10008 server_asid=1 record_index=1 record_generation=17 detached=1 result=ok
IPC_SERVER_DEATH_DEFERRED_PUBLISHED server_tid=10008 server_asid=1 record_index=1 record_generation=17 cpu=0 items=1 result=ok
IPC_SERVER_DEATH_POST_LOCK_DRAIN_BEGIN cpu=0 record_index=1 record_generation=17 items=1 broad_lock=0 result=ok
```

The reverse link was created by an **ordinary queued** call, captured by the exiting server's
`exit_task`, published to the per-CPU deferred queue, and drained after the broad lock released
— carrying the correct `{tid, asid}` and record generation end to end.

## Why the cell was not earned

The drain rejected the item, and it was **right** to:

```text
IPC_SERVER_DEATH_WRONG_SERVER_IDENTITY   armed_tid=0 armed_asid=0 item_tid=10008 item_asid=1 record_index=1 caller_wakes=0 result=fail
IPC_SERVER_DEATH_WRONG_RECORD_GENERATION armed_generation=0 item_generation=17 record_index=1 caller_wakes=0 result=fail
```

`armed_tid=0 armed_asid=0 armed_generation=0` is `TerminalIdentity::ZERO` — the **vacant** cell.
The queued item is correct; there was nothing armed to match it against.

The cause is one unwired arm:

> `maybe_arm_reply_timeout_oracle` is the ONLY production caller of `arm_reply_terminal`. Its
> `deadline_tick` match has arms for `IPC_REPLY_TIMEOUT_MODE_TIMEOUT_WINS` and
> `IPC_REPLY_TIMEOUT_MODE_REPLY_WINS`, then `_ => return`. **`IPC_REPLY_TIMEOUT_MODE_SERVER_DIES`
> falls into that `_`**, so in ServerDies mode the function returns before arming the terminal
> cell, before registering the deadline, and before recording the stale timeout token.

The live log carries **zero** `IPC_REPLY_TIMEOUT_ARM*` markers, confirming the gate returned
early. Two of this stage's requirements therefore cannot be met at all yet:

* **PeerDeath → `ServerDied=10` → caller enqueue → userspace validation** — the completion
  transaction never runs, because the terminal claim has no armed identity to claim against.
* **stale timeout scanned and rejected** — no deadline is registered, so no stale token is
  recorded for the scan to examine. (`record_server_dies_stale_token` exists from Stage
  200D-2B1A and has its call site at the arm site — which this mode never reaches.)

## Scope note

This is a **production wiring gap, not a boot/invocation defect**. The stage authorised fixing
genuine boot/invocation defects only; the runner was run unchanged and required no correction
this time. Adding a ServerDies arm means choosing the scenario's deadline policy and arming the
terminal cell, the deadline token and the stale-token record together — real kernel work for a
follow-up stage, of the same shape as 200D-2B1D1.

Nothing was weakened to get further: the marker chain, its ordering, the forbidden set, the
identity checks, the wake and PeerDeath counts and the binary audits are exactly as prepared.
The two `WRONG_*` literals that appear are the mechanism **correctly failing closed**, and the
runner correctly treats them as fatal.

## RUN_A

Passed on this tree: `oracle_literals_present=0`, `production_literals_missing=0`.

## Progress against the sealing requirements

| Requirement | Status |
|---|---|
| server exits through NR16 and never returns | ✅ live |
| reverse-link capture | ✅ live (new in 200D-2B1D1) |
| deferred publish + post-lock drain | ✅ live |
| PeerDeath claim | ❌ blocked — terminal cell never armed |
| `ServerDied=10` publication | ❌ blocked |
| exactly one caller enqueue/wake | ❌ 0 |
| userspace validation | ❌ blocked |
| stale timeout scanned and rejected | ❌ blocked — no deadline armed |

## Status

```text
STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL arch=x86_64 result=fail
```

No seal is emitted by this stage.
