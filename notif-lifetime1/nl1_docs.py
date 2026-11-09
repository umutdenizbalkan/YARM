#!/usr/bin/env python3
"""NOTIF-LIFETIME1 doc commit: contract, rules, superseded notes, defect resolution, stage record.
Usage: nl1_docs.py <wt> <record-file>"""
import pathlib
import sys

wt = pathlib.Path(sys.argv[1])
record = pathlib.Path(sys.argv[2]).read_text()


def patch(rel, old, new):
    p = wt / rel
    s = p.read_text()
    assert s.count(old) == 1, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))


SUPERSEDED = (
    "> **Superseded by NOTIF-LIFETIME1 (2026-11-09).** Notification caps are not single-owner: "
    "the UART IRQ witness mints a third cap into init, and delegation, fork inheritance and IPC "
    "transfer all copy them. A Notification now lives while any live capability names its exact "
    "`{index, generation}`; removing a capability gives back only its own reference, and the "
    "last removal destroys the object. See `doc/IPC.md` §8.8.\n"
)

# ── IPC.md §8.8 ──
patch(
    "doc/IPC.md",
    "by the race only, and a refusal is fail-closed unless its unwind restored it.\n\n---\n\n## 9. Authoring rule\n",
    """by the race only, and a refusal is fail-closed unless its unwind restored it.

### 8.8 Notification lifetime (NOTIF-LIFETIME1)

**Contract.** A Notification is a shareable IPC object. The incarnation `{index, generation}`
exists while at least one live capability names it. Removing a capability gives back that
capability's reference and nothing more. Revocation still removes the capability's delegation
subtree under the existing rules, and each removed capability gives back its own reference. The
object is destroyed only when the last reference goes. A capability naming any other generation
of the slot cannot destroy, modify or decrement the incarnation that occupies it. This holds for
SIGNAL and RECEIVE caps, same-process copies, cross-process delegation, fork inheritance and IPC
cap transfer. Holding a capability does not confer destruction of an object others still hold.

**Accounting.** `IpcSubsystem::notification_refs[i]` counts the references on the incarnation
whose generation is `notification_generations[i]`. It lives with the object at IPC rank 3 and is
changed only by the two bodies in `src/kernel/boot/notification_lifetime.rs`:

* **reserve** runs at rank 3 *before* a capability is inserted at rank 4. It refuses an empty
  slot (`WrongObject`) and a different generation or a zero count (`StaleCapability`). Zero is
  terminal, so no publication can resurrect a destroyed object.
* **release** runs at rank 3 *after* a capability was removed at rank 4, and is THE destruction
  owner. In one rank-3 critical section it compares the generation, decrements, and at zero
  clears the IRQ routes naming the slot, takes the waiter, removes the object and advances the
  generation once. A mismatch, an empty slot or an over-release changes nothing. The retired
  waiter is woken after rank 3 is released, through the existing exact-identity wake.

Because a reference is always taken before its capability can be seen and given back only
after it has gone, the count is never below the number of visible capabilities. Ranks 3 and 4
are never held together. The linearization points are:

* publication: the rank-3 reserve;
* removal: the rank-4 slot clear;
* destruction: the rank-3 release that reaches zero.

A removal whose release is still owed only keeps the object alive a little longer.

**Owners.**

* *Insertion* — each reserves first and gives the reference back on every refusal:
  `create_notification` (one creation hold for the duration of its two mints),
  `mint_capability_in_cnode`, the split mint seam `mint_capability_with_memory_ref_split`
  (which carries transfer materialization), broad delegation (also fork and spawn), and the split
  spawn owner's mint and delegation.
* *Removal* — each reports exactly what it removed and releases it:
  `CapabilitySpace::revoke_reporting` (root and every derived descendant), the broad revoke, leaf
  delete and direct revoke, both split revoke commits, the split mint rollback (only for a slot
  it actually cleared), the provisional-cap release, `release_delegation` (broad and split), the
  broad cnode cleanup, and the no-alloc reap that drops a cspace whole.
* The process-cnode grant is released only before anything is minted into its space.

`kernel::boot::tests::notif_lifetime1::every_cspace_mutation_site_is_an_accounted_owner` pins the
site counts and the reserve-before-publish order.

**A transfer in flight holds no reference.** Materialization delegates from the sender's
capability. If the sender has given up every capability before the receiver materializes, the
object is gone and nothing is materialized.

**Coverage.** Hosted, broad and split:

* `irq_generation1` G1–G6 plant a capability the accounting never counted, because no
  production path can make one, and prove the owner is generation-exact;
* `irq_generation1_lifetime` L1–L3: a second holder's revoke or exit leaves the first
  holder's object;
* `notif_lifetime1`: transfer, recursive revocation, failed mint / delegation / fork /
  creation, the reserve/remove interleavings, stale publication onto a reused slot, and the
  site census.

Every step checks that references equal the live capabilities naming the incarnation. Live
coverage comes from the UART/PL011 IRQ witnesses, the only production creator: creation, the
cross-process mint into init, the bind and delivery. No default image creates a Notification,
and no live path revokes or reaps a Notification holder.

---

## 9. Authoring rule
""",
)

# ── CAPABILITY_MODEL.md §7 ──
patch(
    "doc/CAPABILITY_MODEL.md",
    "## 8. No cap materialization under IPC lock\n",
    """### 7.1 Notification capabilities carry a reference (NOTIF-LIFETIME1)

Every cspace site that inserts or removes a capability is an accounted owner. A Notification
capability takes its object's reference at IPC rank 3 *before* it is inserted at rank 4, and
gives it back *after* it is removed. A new insertion or removal site must do the same, or the
`every_cspace_mutation_site_is_an_accounted_owner` census fails. The contract and its owners are
in `doc/IPC.md` §8.8. No other object class gained a reference count.

## 8. No cap materialization under IPC lock
""",
)

# ── AI_AGENT_RULES §14.3 ──
patch(
    "doc/AI_AGENT_RULES.md",
    "  a gate pass without attributing every emitter first.\n",
    """  a gate pass without attributing every emitter first.
- Notification lifetime is capability-referenced (NOTIF-LIFETIME1, `doc/IPC.md` §8.8). Never
  destroy a Notification by slot index or because one capability went. Reserve its reference
  before a capability becomes visible and release it after the capability is gone, through
  `notification_lifetime`.
""",
)

# ── KERNEL_LOCKING superseded notes ──
patch(
    "doc/KERNEL_LOCKING.md",
    "**Notification caps are single-owner per object: revoke → destroy immediately.**\n",
    "**Notification caps are single-owner per object: revoke → destroy immediately.**\n\n" + SUPERSEDED,
)
patch(
    "doc/KERNEL_LOCKING.md",
    "### 42.6 Deferred (Stage 24)\n",
    SUPERSEDED + "\n### 42.6 Deferred (Stage 24)\n",
)

# ── KERNEL_TEST_RULES Rule N+29 ──
patch(
    "doc/KERNEL_TEST_RULES.md",
    "the object. Tests must cover the revoke teardown, its idempotence, the lock-rank\nseparation outcomes, and stale-cap stability.\n",
    "the object. Tests must cover the revoke teardown, its idempotence, the lock-rank\nseparation outcomes, and stale-cap stability.\n\n" + SUPERSEDED
    + "> Stage 22/23 tests now give back the partner capability first, so the revoke under test is\n"
    "> the incarnation's last reference.\n",
)

# ── Deferred defects: resolution of the two IRQ-GENERATION1 entries ──
patch(
    "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md",
    "or adopt a different lifetime model | src/kernel/boot/capability_lifecycle_state.rs, src/kernel/boot/mod.rs (`provision_init_uart_irq_witness`), src/kernel/boot/thread_state.rs, src/kernel/syscall/fork_txn.rs | found during IRQ-GENERATION1\n",
    "or adopt a different lifetime model | src/kernel/boot/capability_lifecycle_state.rs, src/kernel/boot/mod.rs (`provision_init_uart_irq_witness`), src/kernel/boot/thread_state.rs, src/kernel/syscall/fork_txn.rs | found during IRQ-GENERATION1\n"
    "- 2026-11-09 | RESOLVED (NOTIF-LIFETIME1) — both IRQ-GENERATION1 entries above. The owner chose a capability-reference lifetime. Notification objects now live while any live capability names their exact incarnation, and one generation-checked owner destroys them at the last reference, on the broad and the split paths (`doc/IPC.md` §8.8). G1–G6 and L1–L3 pass with the new contract | src/kernel/boot/notification_lifetime.rs | resolved in NOTIF-LIFETIME1\n",
)

# ── KERNEL_UNLOCKING stage record ──
patch(
    "doc/KERNEL_UNLOCKING.md",
    "\n## U6-FRAME (canonical 199C) — EXACT BLOCKED-SEND ENVELOPE PRESERVATION\n",
    "\n" + record + "\n## U6-FRAME (canonical 199C) — EXACT BLOCKED-SEND ENVELOPE PRESERVATION\n",
)
print("docs applied")
