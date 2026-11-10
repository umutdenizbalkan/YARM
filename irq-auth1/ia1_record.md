#### IRQ-AUTH1 (from delivered `41403e1`)

**Findings.** All were confirmed on `41403e1` with B1–B10, which use real binds, revokes, creates
and deliveries, plus a test-only hook between the notification resolve and the route write.

| Test | Result on `41403e1` | Defect or behaviour |
|---|---|---|
| B1 | fails | A Notification SIGNAL capability alone bound any line; no line authority was required |
| B2 | fails | A raw line number was trusted, so authority for line X bound line Y |
| B3, B9 | fail | An occupied line was redirected silently |
| B7 | fails | Kernel-owned lines were bindable (x86 line 0 is the timer vector) |
| B5, B6 | fail | The Notification was validated in one IPC acquisition and the route written in another. A stale capability bound a reused slot, and a destroyed incarnation was left with a route |
| B8 | fails | No binding ended when its authority was revoked |
| B4 | passes | Same-incarnation rebinding was already idempotent |
| B10 | passes | `create_notification`'s sanitize prevented an *existing* route being inherited by a reused slot (B5 covers the in-window case) |

The broad delivery read the route and signalled in two acquisitions, so it could signal a reused
slot.

**Authority audit.**

* No public path mints an IRQ capability. No syscall constructs `CapObject::Irq`.
* The only syscall mint that copies an existing capability (`attenuate_transfer_cap_for_recv_intent`)
  intersects rights.
* Fork never inherits IRQ capabilities.
* `mint_irq_cap*` are kernel-internal, and grants cannot widen rights.

**Repair.** The contract is in `doc/IPC.md` §8.10; the owner is `irq_binding.rs`.

* *Binding.* `bind_irq_notification(irq_cap, notification_cap)` resolves both capabilities in the
  caller's cnode in one rank-4 snapshot, then applies the per-port `FIRST_DEVICE_IRQ_LINE`.
* *One rank-3 decision.* It checks the exact incarnation and the occupancy, then publishes:
  * the same incarnation is idempotent;
  * any other target is refused (`EndpointQueueFull`);
  * a dead owner's record may be replaced exactly.
* *The route.* Each route holds `{incarnation, owner {cnode, CapId}}`.
* *The lease.* It is released through the NOTIF-LIFETIME1 removal report, which now records IRQ
  lines and is revalidated by both releasers. A post-publication rank-4 recheck retracts a bind
  whose owner was removed concurrently.
* *Delivery.* Both paths check the route's generation in their signalling acquisition.
* *Not added:* no syscall, capability type, rights bit, lock or Notification reference.
* *Witness provisioning.* It mints line authority internally; init holds RECEIVE only.

**Commits.**

* code `82995cf`;
* regressions `61164db` and `578daf4`.

**Interrupt queue-full investigation.** A full Notification queue returns `EndpointQueueFull`.

* What holds: the interrupt is acknowledged first, so there is no storm and no unacknowledged
  source. No trapframe is written back and no scheduler transition happens.
* What fails: for an interrupt taken from user or idle context, every port treats the error as
  fatal and halts. Kernel threads log and continue.
* This is recorded as a confirmed defect, not repaired, with IRQ-DELIVERY1 proposed. It is
  reachable only with a bound route, which today exists only in the default-off witness.

**Qualification of `578daf4`.** The schedule was predeclared, the steps ran sequentially and none
was retried. Logs, rcs, artifact bindings and hashes are kept.

The session worker restarted during M2, which killed the runner. M2's partial log is kept
(`M2_server_dies_x86_64.interrupted.log`). The remaining steps M2–Z ran once each through
`ia1_matrix_resume.sh`, with the frozen commands and a recorded hash.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings, identical classes to `41403e1` |
| hosted lib (serial); integration (`--no-fail-fast`) | 6360 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: irq_auth1, irq, notification, notif_waiter1, notif_lifetime1, irq_generation1, stage21, stage22, stage23, u9f_split, ipc_displace1, driver, deleg, revoke, reap, transfer, fork, spawn | 15, 108, 50, 19, 16, 15, 11, 10, 6, 33, 19, 20, 46, 78, 47, 203, 93, 138 passed |
| lock order, common test set | 502257 vs 502143 (see the attribution below) |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V | `IRQ1_WITNESS_PROVISION_OK`; `IRQ_ROUTE_BOUND` line 4 / 33 / 10 to notification 0 generation 1; 8/8 deliveries each; no route retraction, release or reclaim marker in any QEMU log |
| IPC-DISPLACE1 live witness SMP1 ×3 | `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on clean default bindings, F (kernel `ae876b68`) and B (`7fc7c02b`) | smp2-core and smp4-core pass on both. sender-wake ×2 and sched-timeout fail with the inherited signatures, identically on both |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |

*Lock-order attribution.* Per-test attribution is appended to the evidence, not a retry of H6.

* 78 of the 79 changed tests bind IRQ lines. Their changed pairs (`capability←boot_config` +97,
  `ipc←boot_config` −96, `ipc←capability` +46, `task←capability` +132, `task←ipc` −60) are the
  predeclared bind, mint and revalidation acquisitions.
* `scheduler←ipc` −2 / `scheduler←capability` +2 come from two trap-entry tests. The first
  scheduler acquisition after a bind now follows the capability recheck. This is outside the
  predeclared domain list, which omitted the acquisition that follows a bind, and it nets to zero
  per test.
* `scheduler←task` −5 is mostly the race test `c09_reply_vs_timeout_terminal_cas_race`, which
  varies 71–88 between runs of the unchanged baseline.
* The closure guard still proves nothing is nested.

**Hosted negative controls.** Each removes one protection, and all 12 were detected. On the
baseline tree (`41403e1` plus only the test hook), 8 of the 10 reproducers fail.

| Control | Protection removed | Detected by |
|---|---|---|
| NC1 | IRQ capability check (any capability names a line) | B1, B9 |
| NC2 | the route lands on the capability's own line | B2 and 57 more |
| NC3 | occupied-line refusal | B3, B9, reclaim and teardown owner tests |
| NC4 | generation check in the decision | B5 |
| NC5 | validation and mutation in one acquisition (moved before the window) | B5, B6 |
| NC6 | removals report IRQ lines (owner revocation) | both B8 tests |
| NC7 | exact retraction (a stale teardown clears a replacement) | stale-teardown owner test |
| NC8 | kernel-owned line refusal | B7 |
| NC9 | post-publication owner recheck | B9 |
| NC10 | broad delivery generation check | B10 |
| NC11 | split delivery generation check | B10 |
| NC12 | owner liveness before a reclaim | reclaim test, B3, B9 |

**Limits.**

* No syscall binds a line. The negative security tests are hosted; the live proof is the
  provisioning, publication, delivery and acknowledgement on all three interrupt controllers.
* No live path revokes line authority or reaps an owner. Lease release is proven hosted, broad
  and split.
* Two observations are recorded in the deferred-defects list, not repaired:
  * the endpoint-removal check that compares Notification route targets with an endpoint index;
  * the IRQ queue-full halt (proposed IRQ-DELIVERY1).
* The six baseline debts and the NOTIF-WAITER1 sender-waiter observation remain as recorded.

Verdict: **IRQ_AUTH1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-auth1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.
