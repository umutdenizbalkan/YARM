#### ENDPOINT-ROLLBACK1 (from delivered `3148a754`)

**Root cause.** `spawn_ipc_cap_txn::remove_unpublished_endpoint_locked` returned `Published`
whenever `irq_routes.iter().flatten().any(|route| route.notification.index == index)` held. Here
`index` is an ENDPOINT slot, but `route.notification.index` is a NOTIFICATION slot.

* The two classes have independent slot indexes and generations.
* An `IrqRoute` names a Notification incarnation (IRQ-AUTH1), so it can never refer to an Endpoint.
* The check therefore refused the rollback of any provisional endpoint whose slot number equalled
  a routed Notification's.

The rollback revokes the grant's capabilities before it removes the endpoint. A refused removal
therefore left an endpoint that nothing could name and nothing would ever remove: a permanent
endpoint-capacity leak on every such failed spawn. The unwind still logged `result=baseline`.

**Caller and contract audit.**

* *Callers.* There is one policy caller, `spawn_txn::release_service_endpoint_grant`, reached
  from the spawn ledger's `Endpoint` entry (`spawn_image_txn::release_provisional_spawn_resource`).
  The two adapters (broad `BroadSpawnOwners`, split `SharedSpawnOwners`) both call the same rank-3
  body.
* *The token.* The token is the `ServiceEndpointGrant` from `provision_service_endpoint_locked`.
  That transaction installs the incarnation and mints the SEND and RECEIVE capabilities into the
  spawner's cspace, all in one rank-3-then-rank-4 transaction.
* *Revocation first.* Before the removal, both capabilities are revoked through the
  provisional-capability rollback, together with their delegated children. That includes the
  parent's delegated SEND copy, which is a descendant of the send capability.
* *Stale tokens.* A stale token is refused by the exact `{index, generation}` check.
* *No IRQ link.* No object model path lets an IRQ route reference an Endpoint: a route is
  `{notification: NotificationIncarnation, owner}`.
* *External visibility.* The endpoint can become externally visible only through its own
  capabilities: a receiver waiter, a parked sender, or a queued buffered message. The IRQ check
  never guarded any of these, so removing it exposes nothing that was protected before. That rules
  out the hard-stop.
* *Queued messages.* Neither this removal nor `destroy_endpoint` inspects buffered queued
  messages. That is recorded as a separate open observation (reachability for a provisional
  endpoint not established) and is not changed here.

**Repair** (`f540d447`). The IRQ-route comparison is removed and no replacement comparison is
added. These are unchanged:

* the exact incarnation check;
* the receiver-waiter and sender-waiter exclusions;
* generation advancement;
* the `Stale`/`Published`/`Removed` semantics;
* rank-3 ownership;
* the capability-then-endpoint rollback order.

Comments now state that IRQ routes name Notifications. Two existing tests encoded the confusion and
are re-derived:

* the IC1 source guard no longer requires `irq_routes`, and now forbids the removal from
  consulting another object class;
* the IC1 published-endpoint test used to "publish" the endpoint by forcing a route whose
  Notification index equals the endpoint index. It now publishes it with a real blocked receiver.

**Reproducers** (`b9a29032`, `endpoint_rollback1`, all real objects).

* *How the collision is set up.* A Notification is lined up at the exact slot index the next
  endpoint takes, and that coincidence is asserted. Routes are bound through line authority
  (`mint_irq_cap` + `bind_irq_notification`), and events are delivered through the real split
  delivery.
* *E8.* It runs `run_image_spawn_transaction` unmodified through `FailingCommitOwners`. This
  wrapper delegates all 55 owner calls to the real broad owners and fails only
  `bind_spawned_task_asid`, a commit-phase step after both service endpoints and the delegation.
  An entry point of zero no longer serves as the commit failure: the image provisioner now
  refuses it first, before any endpoint exists.

| Test | `3148a754` | Candidate |
|---|---|---|
| E1 same-index route, broad adapter | fails: `Published` | removed; Notification, generation, refs, route and pending event unchanged |
| E1 same-index route, split adapter | fails: `Published` | the same |
| E2 route at another index | passes | passes |
| E3 a real blocked receiver | passes (`Published`) | passes; the waiter and receiver are untouched |
| E4 a real parked capability-bearing sender | passes (`Published`) | passes; waiter, message and live envelope untouched |
| E5 stale token, recycled slot | fails: the first release is `Published` | `Stale`; the replacement is untouched |
| E6 six provision/rollback cycles on the colliding slot | fails at round 0 | slot reused, generation advances, old incarnation unresolvable, no capacity retained |
| E7 three routes, one at the same index | fails: `Published` | removed; IRQ state unchanged |
| E8 the real failed spawn, routes at both service-endpoint indexes | fails: endpoints 0 and 1 retained | four rounds, every resource back, IRQ state unchanged, capabilities still released before the endpoint |

**Resource accounting.** These are checked directly, not inferred from return codes:

* *Endpoints and capabilities* (E6, E8): the live endpoint set, slot reuse and incarnation
  advance, and the spawner's capability count.
* *Tasks and frames* (E8): the task table and free frames.
* *IRQ and Notification state* (E1, E7, E8): routes, every Notification generation and reference
  count, and every queued or pending event, plus delivery of the pending event after the rollback.
* *Published endpoints* (E3, E4): the waiter, the parked message and the live envelope survive a
  refused removal.

**Qualification of `b9a29032`.** This followed a predeclared schedule (`er1_schedule.md`, frozen
by hash with the runner, scripts, patch and tests). The steps ran sequentially, and nothing was
retried or substituted. Logs, rcs, artifact bindings and hashes are kept. The baseline is
`3148a754`.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `3148a754` |
| hosted lib (serial) | 6412 passed, 0 failed, 3 ignored |
| integration (`--no-fail-fast`): broad-lock census, doc-fragmentation guard | 41 binaries, 396 passed |
| subsets: endpoint_rollback1, u9spawnic1_endpoint_cap_txn, u9spawn1_sp3_spawn_ledger, u9spawntxn3_provisional_rollback, u9spawnvm2_provision_failure_injection, spawn, fork, endpoint, generation, capability, exit, reap, send_timeout_arb1, ipc_sender_id1, ipc_displace1, sender_wake, transfer, shared_region, notif_lifetime1, notif_waiter1, irq_generation1, irq_auth1, irq_delivery1 | 9, 14, 11, 12, 5, 139, 93, 180, 88, 97, 318, 47, 12, 25, 19, 91, 204, 178, 16, 19, 15, 15, 15 passed, 0 failed |
| lock order, common test set | 8 changed (test, pair) entries, all predeclared; pair kinds identical (below) |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| UART (x86_64), PL011 (AArch64), UART (RISC-V) IRQ witnesses | `result=ok`; 8 of 8 deliveries each, through live Notification-bound routes |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on clean default bindings, F and B: sender-wake, d2-send, smp2-core, smp4-core, smp2/smp4-sender-wake, sched-timeout | identical status and identical error sets on F and B. sender-wake and smp2/smp4-core pass. smp2/smp4-sender-wake and sched-timeout carry inherited #2, #3, #4. d2-send carries the inherited 8 errors |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67) and #6 (0/0/0), identical on F and B |
| every QEMU log of F (84 files) for the marker list below | no hits |

The marker list:

* `SCHED_WAKE_FAIL`, `SCHED_TIMEOUT_STRANDED_WAITER`, `IPC_RECV_BLOCKED_COMPLETE_FAILED`;
* the ports' fatal channels;
* `U6_SEND_COMPLETION_REFUSED_STALE`, `U7_SEND_TIMEOUT_CLAIM_AFTER_WITHDRAW_REFUSED`,
  `SEND_ARB_PUBLICATION_REFUSED_UNWIND`;
* `endpoint=Published`, `outcome=Published`, `SPAWN_LEDGER_OVERFLOW`.

No live run failed a spawn, so no live rollback ran, as predeclared.

*Lock-order attribution.* Every change is in a predeclared class:

* *(a) the re-derived* `a_published_endpoint_is_refused_by_the_unpublished_removal`, which now
  blocks a real receiver: six entries (`ipc←11` +1, `ipc←capability` −1, `scheduler←task` +1,
  `scheduler←ipc` +2, `task←ipc` +2, `task←capability` +1).
* *(b) the run-to-run variant* `c09_reply_vs_timeout_terminal_cas_race`: two entries.
* *(c)* empty, as predicted. The removal takes the same single rank-3 acquisition on both sides.

Nothing is nested.

**Hosted negative controls.** Each control reintroduces one defect in a disposable worktree, and
all six were detected, each by a behavioural test. The unmodified candidate passes all 61 tests
in the subset (`er1_nc.py`, hash recorded).

| Control | Defect reintroduced | Detected by |
|---|---|---|
| NC1 | the index-space comparison restored | E1 ×2, E5, E6, E7, E8, the IC1 guard |
| NC2 | the endpoint generation check removed | E5, IC1 stale-grant |
| NC3 | the receiver-waiter guard removed | E3, IC1 published-endpoint, the IC1 guard |
| NC4 | the sender-waiter guard removed | E4, the IC1 guard |
| NC5 | the rollback clears the same-index IRQ route | E1 ×2, E7, E8 |
| NC6 | the generation not advanced on removal | E1 ×2, IC1 grant-and-release |

**Limits.**

* *Hosted proof only.* The repaired branch runs only when a spawn fails after its endpoint phase.
  No live boot path does that, so the proof is hosted (both adapters, and the real spawn
  transaction). The live runs are regression evidence for spawn, IPC and Notification-bound IRQ
  routes on three ports.
* *Not changed:* `KERNEL_UNLOCK_AUDIT.md`'s U9-SPAWN-IC1 record is historical and still lists four
  checks; the current-state descriptions (`PROCESS_AND_SPAWN.md`, `STATUS.md`) are updated.
* *Still open:* the six baseline debts and the inherited `d2-send` failure remain as recorded. A
  new open observation records that neither teardown settles queued buffered messages.
* *Closed:* the IRQ-AUTH1 endpoint-removal observation is resolved.

Verdict: **ENDPOINT_ROLLBACK1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/endpoint-rollback1`
at the commit that adds this record; `main` `b81e0317` and every other ref unchanged.
