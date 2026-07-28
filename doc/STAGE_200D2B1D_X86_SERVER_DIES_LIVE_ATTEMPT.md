<!-- SPDX-License-Identifier: Apache-2.0 -->

# Stage 200D-2B1D-x86 — first ServerDies live attempt (NOT sealed)

The x86_64 exact-commit runner was executed against `server-dies|3`. **No live cell is
claimed.** The boot exposed a production wiring gap that no hosted test could have caught,
and the run is recorded here so the next stage starts from the diagnosis rather than
repeating it.

## What the live boot proved

The oracle provisioned and ran, and the **architecture return contract completed correctly**
on real hardware emulation:

```text
IPC_REPLY_TIMEOUT_ORACLE_BEGIN / PROVISION_OK / SERVER_STARTED / CLIENT_CALL_OK
IPC_SERVER_DEATH_REQUEST_RECEIVED          server received the request
IPC_SERVER_DEATH_REPLY_CAP_RECEIVED        with a live reply cap
IPC_SERVER_DEATH_EXIT_ENTERED  nr=16       server entered the real NR16
EXIT_TASK_SYSCALL_DISPATCHED / PREFLIGHT_OK / LIFECYCLE_TRANSITION
IPC_SERVER_DEATH_DEFERRED_RESERVED         cpu=0 slots=1 result=ok
EXIT_TASK_DISPOSITION_PUBLISHED            tid=10008 asid=1
EXIT_TASK_DISPOSITION_CONSUMED             the x86_64 consumer ran
EXIT_TASK_EXITING_NOT_CURRENT              exiting task is not current
EXIT_TASK_RESTORE_OWNER_PREPARED           a restore owner was named
EXIT_TASK_POST_LOCK_DRAIN_DONE             drains completed after the lock
EXIT_TASK_COMMON_EPILOGUE_OWNER            single epilogue, single depth clear
```

The server exited through NR16 and **never returned** — `IPC_SERVER_DEATH_EXIT_RETURNED` is
absent. So Stage 200D-2B1C's return-contract half is live-confirmed on x86_64.

## Why the cell was not earned

The chain stops immediately after `DEFERRED_RESERVED`. `IPC_SERVER_DEATH_LINK_CAPTURED` never
appears, which means `take_server_reply_link` found **nothing to detach**.

The cause is a single fact:

> The only production call site that CREATES a `ServerReplyLink` is
> `src/kernel/ipccall_direct_txn.rs:358` — inside the IpcCall-**DIRECT** transaction.
> The live oracle used the **ordinary queued** call path.

The boot log carries `IPC_CALL_BEGIN`, `IPC_CALL_REPLY_CAP_CREATE`, `IPC_CALL_SENT_OR_QUEUED`,
`IPC_CALL_SPLIT_DELIVERY` and `IPC_CALL_WAKE_RECEIVER`, and **no** direct-transaction marker at
all. On the ordinary path no reverse link is ever registered, so `exit_task` reserves a
deferred slot, finds no link, and the entire server-death chain — PeerDeath claim, canonical
`ServerDied`, caller enqueue, userspace validation — cannot begin.

`grep -rn "register_server_reply_link_split(" src/` returns exactly one non-definition hit,
in the direct-transaction file. There is no second registration site.

## Why the Stage 200D-2B1B foundation did not catch it

Every one of the 24 hosted races registers the link **explicitly**, through the fixture:

```rust
fn link(fx: &CallerFx) -> bool {
    fx.k.with(|s| s.register_server_reply_link(...))
}
```

So the foundation proved, correctly and thoroughly, everything **downstream of an existing
link** — the reserve/detach/publish ordering, the post-lock drain, the single terminal winner,
the canonical code, the fail-closed rejections. It proved nothing about the link's own
creation on the ordinary call path, because it never exercised that path. That is exactly the
"guard wiring, not declarations" gap this project keeps finding, one level up: the transaction
was guarded, its entry condition was not.

The 2B1B seal remains accurate for what it claims (a hosted foundation, `live_cells=0`), but
it should be read with this scope limit in mind.

## What the next stage needs to do

Register the reverse link on the **ordinary** reply-cap path, at the point where the record
becomes authoritative and before the server is enqueued — the same ordering the direct
transaction already uses and documents. Then re-run this runner unchanged.

That is production kernel work and was out of scope here: this stage was scoped to running the
runner and adjusting only genuinely necessary QEMU machine/console arguments.

## Runner corrections made (grading untouched)

Four invocation defects, all found by running it, none in the proof:

| Defect | Fix |
|---|---|
| `-initrd` missing — the oracle lives in `init_server` inside `initramfs-core.cpio`, so the scenario could never run | boot the staged initramfs |
| raw `target/` ELF instead of the staged, PVH-noted `build-x86_64/kernel_boot.elf` | stage via `build-qemu-x86_64-artifacts.sh` |
| no `-serial stdio -monitor none`, no `console=ttyS0` — kernel output never reached the log | added |
| no `rdinit=/init` — init_server never started | added |

Two further bugs surfaced in the staging step itself and were fixed:

* the artifact script rebuilds the kernel **feature-off** into the same target path, so the
  oracle-on kernel must be rebuilt **after** it, not before;
* `strings … | grep -q …` under `set -o pipefail` fails precisely when the literal **is**
  present (grep exits early, strings takes SIGPIPE), which made a correctly staged kernel look
  un-staged.

The marker chain and its ordering, the 18-literal forbidden set, the identity checks, the
exactly-one caller wake and exactly-one PeerDeath winner, and the two-sided binary audit were
**not** touched. RUN_A passed on this tree: `oracle_literals_present=0`,
`production_literals_missing=0`.

## Status

```text
STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL arch=x86_64 result=fail
```

No seal is emitted by this stage. The runner is now known-good up to the point of the gap, and
will grade a real chain as soon as the ordinary path registers the link.
