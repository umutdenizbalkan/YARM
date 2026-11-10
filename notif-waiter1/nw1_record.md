#### NOTIF-WAITER1 (from delivered `dc5c8fd`)

**Findings.** All were confirmed with real receive, signal, timeout, revoke and exit operations
and deterministic phase handshakes. Each fails on `dc5c8fd`.

* **N1, overwritten live waiter.** The blocking notification receive published its record with
  an unconditional write. A second receiver replaced the first, which stayed
  `Blocked(EndpointReceive)` with no record: nothing could wake it except destruction or exit.
* **N2, signal between the empty check and publication.** A signal in that window queued the
  event and found no waiter, and the receiver then slept with the event pending. Under
  concurrent execution the interleaving needs the split IRQ delivery, which takes rank 3 only
  and no global lock, on another CPU.
* **N3, generation change before publication.** Neither the lifetime model nor serialization
  prevented it. An incarnation destroyed and reused in that window received the old
  receiver's waiter.
* **Timeout in the window.** A receive timeout settled in the same window left a stale record.
* **Bare-TID removal.** Every notification-waiter removal matched the bare TID (task teardown,
  broad timeout scan, reap and exit claim body, split receive-timeout settle). A record of
  another `{tid, asid}` incarnation with the same TID was removed.

**Reachability.** No syscall reaches the blocking notification receive on any ISA or build:

* NR 2 and NR 5 validate an endpoint capability first;
* the witness builds' only notification door is the non-blocking NR 5 probe.

So all five findings are confirmed in the kernel primitive and are not reachable in
production today. Waiting on a notification is neither production-default nor witness-reachable.
The live UART/PL011 witnesses exercise creation, binding and signal delivery only.

**Repair.** The contract is in `doc/IPC.md` §8.9. The owners live in
`notification_waiter.rs` and run at IPC rank 3:

* a generation-checked immediate take, keeping the receive's error precedence;
* after the block, ONE publication decision, which may only:
  * refuse a gone incarnation (`StaleCapability`);
  * consume a queued event;
  * refuse a competing receiver (`EndpointQueueFull`), unless rank 2 proves its record stale;
  * publish.

Around that decision:

* Every unpublished outcome restores the receiver through `wake_tid_to_runnable` +
  `dispatch_next_task`, the IPC-DISPLACE1 refusal shape.
* A published receiver that was settled in the window has its exact record retracted.
* All removals match the complete `{tid, asid}`.

No syscall, capability type, lock, queue or `WaitReason` was added. The signal side and
NOTIF-LIFETIME1 are unchanged, and the waiter holds no reference.

Commits:

* code `2e88ac9`, plus `8c31d9e`, which keeps `WrongObject` for an empty slot;
* regressions `7415a43`.

**Qualification of `8c31d9e`.** This followed a predeclared schedule. The steps ran
sequentially and none was retried. Logs, rcs, artifact bindings and hashes are kept. A first
run on `7415a43` was stopped by the operator after its hosted steps, to restore the error
precedence above; its logs are kept as `qual-aborted-7415a43`.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `dc5c8fd` |
| hosted lib (serial); integration | 6345 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: notif_waiter1, notification, notif_lifetime1, irq_generation1, stage22, stage23, u9f_split, ipc_displace1, exit_task, timeout, fork, deleg, transfer, revoke, reap, spawn | 19, 49, 16, 15, 10, 6, 33, 19, 10, 160, 93, 46, 203, 77, 46, 138 passed |
| lock order (hosted sequential tracker), common test set | 500469 vs 500448. The changes match the revised predeclaration: `task←ipc` +18 (the post-publication rank-2 read); `scheduler←task` +3 (the refusal unwind); `ipc←boot_config` −79 / `ipc←capability` +79 (the capacity-config read before the take is gone). Nothing is nested |
| default builds ×3; recv-v2 oracles ×3 | pass |
| CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| UART/PL011 IRQ witness ×3 | `IRQ1_WITNESS_PROVISION_OK`, 8/8 deliveries each; no `NOTIF_REF_*` or `NOTIF_WAITER_*` in any QEMU log |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 (live teardown + reap through the converted bodies) | pass |
| CI profiles on clean default bindings, F (kernel `7fc7c02b`) and B (`9bcb98ea`) | smp2-core and smp4-core pass on both. sender-wake ×2 and sched-timeout fail with the inherited signatures, identically on both |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |

**Hosted negative controls.** Each removes one protection, and all 11 were detected. On the
baseline tree (`dc5c8fd` plus only the test hook, the unused owner module and the tests), 14
of the new tests fail.

| Control | Protection removed | Detected by |
|---|---|---|
| NC1 | publication ignores the generation | N3, new-incarnation test |
| NC2 | no queue re-check at publication | N2, split-signal-in-window test |
| NC3 | publication overwrites any record | N1 and 5 more |
| NC4 | no post-publication retract | timeout-in-window test |
| NC5 | reap/exit claim body clears by bare TID | recycled-identity test, structural guard |
| NC6 | task teardown clears by bare TID | exit companion, structural guard |
| NC7 | immediate take ignores the generation | take owner test |
| NC8 | a stale competing record is never reclaimed | supersession/reclaim test, lifetime test |
| NC9 | split receive-timeout settle clears by bare TID | structural guard only |
| NC10 | a refused receiver is not restored | 9 tests, including the wake-owner census |
| NC11 | same-receiver supersession admits an equal or older generation | supersession owner test |

**Limits.**

* The repair is proven hosted. No live path blocks on a notification, and none can without new
  ABI, which is out of scope.
* The cross-CPU tests enter the off-lock split delivery for CPU 1 inside the receive's window
  on a hosted kernel. They show the interleaving and the wake. They do not show placement on a
  second online CPU.
* NC9 is detected only structurally.
* The endpoint SENDER waiter sweeps beside the converted clears still match the bare TID. They
  are not receive waiters and are recorded as an open observation.
* Out of scope and unchanged:
  * multi-receiver FIFO;
  * IRQ binding authority (IRQ-AUTH1).
* The six baseline debts remain as recorded.

Verdict: **NOTIF_WAITER1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/notif-waiter1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.
