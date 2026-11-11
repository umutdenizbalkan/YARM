#!/usr/bin/env python3
"""ENDPOINT-ROLLBACK1 documentation commit. Usage: er1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

# ── doc/IPC.md: §8.14 ──────────────────────────────────────────────────────────────────────
IPC814 = '''### 8.14 Provisional endpoint rollback (ENDPOINT-ROLLBACK1)

A failed spawn gives back its service endpoints through
`spawn_ipc_cap_txn::remove_unpublished_endpoint_locked`. The policy
(`spawn_txn::release_service_endpoint_grant`) first revokes both of the grant's capabilities,
together with their delegated children, and only then removes the endpoint.

The removal consults only the endpoint's own state:

* **The exact incarnation:** `endpoints[index]` is occupied and `endpoint_generations[index]`
  equals the token's generation. Otherwise the result is `Stale`: the token owns nothing, and a
  replacement in the slot is never touched.
* **No receiver waiter:** `endpoint_waiters[index]` is empty.
* **No parked sender:** `endpoint_sender_waiters[index]` is empty.

If either waiter check fails, the result is `Published` and nothing is mutated; the endpoint then
belongs to `destroy_endpoint`. Otherwise the slot is cleared and the generation advances.

**Object namespaces are not compared.** An IRQ route names a Notification incarnation
`{index, generation}` (§8.10). Endpoints and Notifications have independent slot indexes and
generations, so a route can never refer to an endpoint, and the removal does not consult
`irq_routes`.

Until ENDPOINT-ROLLBACK1 the removal refused any endpoint whose slot number equalled a routed
Notification's index. A failed spawn then kept that endpoint allocated for good, while its
capabilities were already revoked and nothing could ever name it again.

**Not changed:**

* `IrqRoute`, Notification lifetime and delivery, and binding ownership;
* the rollback order (capabilities, then the endpoint) and the token semantics;
* the rank-3 ownership of the removal;
* `destroy_endpoint`.

**Coverage.** `kernel::boot::tests::endpoint_rollback1` uses real Notifications bound to real lines
through line authority, real deliveries, a real blocked receiver and a real parked
capability-bearing sender. It covers:

* E1: a same-index route, through the broad and the split adapters;
* E2: a route at another index;
* E3: a blocked receiver;
* E4: a parked sender with its envelope;
* E5: a stale token;
* E6: repeated reuse of one slot;
* E7: several routes;
* E8: the real spawn transaction, failing at its commit through a failure-injecting owner set.

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC814)

# ── current-state descriptions of the removal ───────────────────────────────────────────────
patch("doc/PROCESS_AND_SPAWN.md", '''still matches and the endpoint is still unpublished — no waiting receiver, no
parked sender, no IRQ route. A published endpoint belongs to `destroy_endpoint`,''',
'''still matches and the endpoint is still unpublished — no waiting receiver and no
parked sender. (IRQ routes name Notifications, never endpoints, so they are not
consulted: `doc/IPC.md` §8.14.) A published endpoint belongs to `destroy_endpoint`,''')
patch("doc/STATUS.md", '''matches AND no receiver waits, no sender is parked and no IRQ routes to it. Destroying a LIVE''',
'''matches AND no receiver waits and no sender is parked (IRQ routes name Notifications, not
endpoints, and are not consulted since ENDPOINT-ROLLBACK1). Destroying a LIVE''')

# ── doc/AI_AGENT_RULES.md ───────────────────────────────────────────────────────────────────
patch("doc/AI_AGENT_RULES.md", '''  `TimedOut` ending without holding the withdrawn waiter. Never release the envelope of a send
  that parked a waiter.
''', '''  `TimedOut` ending without holding the withdrawn waiter. Never release the envelope of a send
  that parked a waiter.
- Never compare slot indexes across object classes (ENDPOINT-ROLLBACK1, `doc/IPC.md` §8.14).
  Endpoints, Notifications and Replies have independent index spaces and generations. A
  reference to an object is its own class's `{index, generation}`: an IRQ route names a
  Notification and can never refer to an Endpoint.
''')

# ── doc/KERNEL_TEST_RULES.md ────────────────────────────────────────────────────────────────
patch("doc/KERNEL_TEST_RULES.md", '''> **SEND-TIMEOUT-ARB1 (2026-11-10).** A window that a production function does not return''',
'''> **ENDPOINT-ROLLBACK1 (2026-11-11).** A cross-object collision is arranged, never assumed:
> line the two namespaces up with real objects (`notification_at_next_endpoint_index`), bind the
> route through line authority, and assert the exact index coincidence. To fail a transaction
> after a phase no production input can fail, wrap the real owners
> (`FailingCommitOwners: SpawnTxnOwners`) and fail exactly one owner call, as an owner may. The
> production policy and its ledger then run unmodified. See `doc/IPC.md` §8.14.

> **SEND-TIMEOUT-ARB1 (2026-11-10).** A window that a production function does not return''')

# ── doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md ───────────────────────────────────────────────────
D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
patch(D, "- 2026-11-10 | OPEN OBSERVATION — `remove_unpublished_endpoint_locked` refuses an endpoint removal when any IRQ route targets a NOTIFICATION slot whose index equals the ENDPOINT index (the two index spaces are compared). It can only refuse a removal, never corrupt a route; preserved unchanged by IRQ-AUTH1 | src/kernel/boot/spawn_ipc_cap_txn.rs | found during IRQ-AUTH1\n",
      "- 2026-11-10 | RESOLVED (ENDPOINT-ROLLBACK1) — `remove_unpublished_endpoint_locked` refuses an endpoint removal when any IRQ route targets a NOTIFICATION slot whose index equals the ENDPOINT index (the two index spaces are compared). It can only refuse a removal, never corrupt a route; preserved unchanged by IRQ-AUTH1. Consequence, confirmed: a failed spawn whose service endpoint landed on such a slot kept that endpoint allocated for good, after both of its capabilities were already revoked, so nothing could ever name or remove it (endpoint capacity leak). Resolution: the removal consults only the endpoint's own incarnation, receiver waiter and parked senders; a route names a Notification and is not consulted (`doc/IPC.md` §8.14). Confirmed by E1, E5, E6, E7 and E8 failing on `3148a754` | src/kernel/boot/spawn_ipc_cap_txn.rs | found during IRQ-AUTH1; resolved in ENDPOINT-ROLLBACK1\n"
      "- 2026-11-11 | OPEN OBSERVATION — neither `remove_unpublished_endpoint_locked` nor `destroy_endpoint` inspects an endpoint's BUFFERED message queue: dropping the endpoint drops queued messages, and a queued message that carries a transfer envelope is not settled. For a provisional service endpoint this matters only if a message can be queued on it before a failed spawn's rollback (for example through the parent's delegated SEND capability between the delegation and the commit). That reachability is not established. It is independent of the IRQ-route comparison removed in ENDPOINT-ROLLBACK1, which never protected against it | src/kernel/boot/spawn_ipc_cap_txn.rs, src/kernel/boot/ipc_state.rs (destroy_endpoint) | found during ENDPOINT-ROLLBACK1\n")

# ── doc/KERNEL_UNLOCKING.md: the stage record ───────────────────────────────────────────────
U = "doc/KERNEL_UNLOCKING.md"
anchor = '''Verdict: **SEND_TIMEOUT_ARB1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/send-timeout-arb1` at
the commit that adds this record; `main` `b81e0317` and every other ref unchanged.
'''
patch(U, anchor, anchor + '\n' + record.rstrip('\n') + '\n')
print("docs applied")
