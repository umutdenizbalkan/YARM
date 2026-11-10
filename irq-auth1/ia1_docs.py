#!/usr/bin/env python3
"""IRQ-AUTH1 documentation commit. Usage: ia1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

IPC810 = '''### 8.10 IRQ binding (IRQ-AUTH1)

**Authorities.** Five things are kept apart:

1. **Signal authority** is a Notification capability with SIGNAL.
2. **Line authority** is `CapObject::Irq { line }` with SIGNAL. It is the line's only source.
   Only the kernel mints one (`mint_irq_cap`, `mint_irq_cap_for_task`), no syscall can, fork
   never inherits one, and delegation (`grant_driver_irq`) cannot widen it.
3. **Binding ownership** is the route's owner: the cnode and the generation-bearing `CapId` of
   the IRQ capability the binding was made with.
4. **The Notification's lifetime** belongs to NOTIF-LIFETIME1 (§8.8). A route holds no
   reference.
5. **The IRQ capability's lifetime** follows ordinary capability revocation and reaping.

**Binding.** `bind_irq_notification(irq_cap, notification_cap)` takes both capabilities. It
resolves them in the caller's own cnode in one rank-4 snapshot, and never takes a raw line
number. The line must be below `MAX_IRQ_LINES` and must not be kernel-owned
(`FIRST_DEVICE_IRQ_LINE`):

* x86_64: line 0 is the LAPIC timer vector;
* AArch64: INTIDs below 32 are SGIs and PPIs;
* RISC-V: PLIC source 0 means "no interrupt".

One rank-3 decision (`bind_irq_route_locked`) checks the incarnation's presence and exact
generation, and the line's occupancy, and then publishes. There is no interval between the
generation check and the route write. An occupied line is never redirected:

* the same incarnation is an idempotent success, which keeps the original owner;
* any other target is refused (`EndpointQueueFull`), unless the occupying owner was proved dead
  at rank 4.

A failed bind changes nothing.

**Lease.** `irq_routes[line]` holds `{notification incarnation, owner}`. It lives exactly while
both are live.

* **When the Notification goes:** its final release clears the routes naming it, inside the
  destroying critical section.
* **When the owner goes:** every capability removal already reports what it removed through
  NOTIF-LIFETIME1's removal report. The report now also records IRQ lines. Both releasers
  (`release_notification_refs`, `_split`) revalidate those lines after rank 4 and clear each
  route whose owner no longer resolves. This covers:
  * revoking the owner;
  * a subtree revoke containing it;
  * the split revoke;
  * the reap that drops the owner's cspace.
* **Revoking a delegated copy or a sibling** leaves the route, because the owner still
  resolves.
* **A concurrent owner removal during a bind:** the bind rechecks its owner after publication
  (rank 4) and retracts its exact record. Either the recheck sees the removal, or the removal's
  revalidation sees the route.
* **Transfer:** a binding is never transferred. Once the old lease ends, a holder of the line's
  authority binds afresh.

A stale capability cannot change a newer binding: retraction and stale replacement act only on
the exact record they were given.

**Delivery.** The broad `route_external_irq` and the split `deliver_external_irq_split` check the
route's generation in the same acquisition that signals. A route can never deliver into a
replacement incarnation.

**Reachability.** No syscall binds a line. The only production caller is the default-off
UART/PL011 witness provisioning. It mints line authority through the internal path, binds with
it, and gives init RECEIVE only.

**Coverage.** `kernel::boot::tests::irq_auth1`:

* B1–B10;
* stale-owner reclaim;
* stale-teardown exactness;
* the provisioning shape;
* a structural guard that only `irq_binding.rs` publishes a route and that both releasers
  revalidate.

The UART/PL011 witnesses bind and deliver live on all three interrupt controllers.

**Not changed.** A full notification queue still propagates `EndpointQueueFull` out of the
interrupt. The port then halts after acknowledging it. This is recorded for IRQ-DELIVERY1
(`doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC810)

patch("doc/AI_AGENT_RULES.md", '''  `{tid, asid}`. A refused receiver is restored through the receive family's unwind, never by
  a status write.
''', '''  `{tid, asid}`. A refused receiver is restored through the receive family's unwind, never by
  a status write.
- IRQ binding needs line authority (IRQ-AUTH1, `doc/IPC.md` §8.10). Never bind from a raw line
  number: a route is published only by `irq_binding`, from an IRQ capability and a Notification
  SIGNAL capability resolved in the caller's cnode, in the one rank-3 decision. An occupied line
  is never redirected. A route is owned by its exact IRQ capability and released through the
  removal report, so a new removal path must record what it removes in that report.
''')

patch("doc/KERNEL_TEST_RULES.md", '''> **NOTIF-WAITER1 (2026-11-10).**''', '''> **IRQ-AUTH1 (2026-11-10).** A test binds an IRQ line with line authority: mint it through
> `mint_irq_cap` (or `bind_irq_line_for_test`, which does exactly that) and pass both
> capabilities to `bind_irq_notification`. A test that forces a defective route uses
> `forced_route_for_test` and names the incarnation it means. Route assertions compare the whole
> `IrqRoute` (incarnation and owner) where ownership matters. See `doc/IPC.md` §8.10.

> **NOTIF-WAITER1 (2026-11-10).**''')

LOCK_NOTE = '''

> **IRQ-AUTH1 (2026-11-10).** `bind_irq_notification` now takes an IRQ capability and a
> Notification SIGNAL capability. Both are resolved in one rank-4 snapshot (capability), then one
> rank-3 decision (ipc) checks the exact generation and occupancy and publishes, and a rank-4
> recheck of the owner follows. A route holds `{incarnation, owner}`. Removals revalidate the IRQ
> lines they report (rank 3 → rank 4 → rank 3, sequential). Delivery checks the route's generation
> in its signalling acquisition. See `doc/IPC.md` §8.10.'''
patch("doc/KERNEL_LOCKING.md", "| `destroy_notification` (Stage 21 new) | ipc (route teardown + waiter clear + object remove + gen bump) | n/a | atomic teardown; returns snapshotted waiter for out-of-lock unblock |\n",
      "| `destroy_notification` (Stage 21 new) | ipc (route teardown + waiter clear + object remove + gen bump) | n/a | atomic teardown; returns snapshotted waiter for out-of-lock unblock |\n" + LOCK_NOTE + "\n")
patch("doc/KERNEL_LOCKING.md", "| slot reuse via `create_notification` | stale routes to slot pre-cleared | no cross-generation retarget |\n",
      "| slot reuse via `create_notification` | stale routes to slot pre-cleared | no cross-generation retarget |\n" + LOCK_NOTE + "\n")

patch("doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md", "| src/kernel/boot/ipc_state.rs, src/kernel/boot/reap_claim.rs | found during NOTIF-WAITER1\n",
      "| src/kernel/boot/ipc_state.rs, src/kernel/boot/reap_claim.rs | found during NOTIF-WAITER1\n"
      "- 2026-11-10 | RESOLVED (IRQ-AUTH1) — `bind_irq_notification` bound any line from a Notification SIGNAL capability with no line authority, redirected an occupied line, accepted kernel-owned lines, validated the Notification and wrote the route in separate IPC acquisitions (a stale capability could bind a reused slot), and no binding ended when its authority was revoked (`doc/IPC.md` §8.10). Confirmed with B1–B9 on `41403e1`; B4 (idempotence) and B10 (a reused slot inherits no existing route, through the create-time sanitize) held already | src/kernel/boot/irq_binding.rs | resolved in IRQ-AUTH1\n"
      "- 2026-11-10 | CONFIRMED, NOT REPAIRED (proposed IRQ-DELIVERY1) — a full Notification IRQ queue makes `deliver_external_irq_split` / `route_external_irq` return `EndpointQueueFull`, which the bridge settles as an error after the one completion is written (the interrupt IS acknowledged: no storm, no unacked source). For an interrupt taken from user or idle context every port then treats the shared-dispatch error as fatal: x86_64 `halt_forever` (`x86 shared trap dispatch failed`), AArch64 `YARM_AARCH64_TRAP_HANDLE halting`, RISC-V `RISCV_TRAP_HANDLE_FAILED`. The kernel-thread paths log `KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED` and continue. A device raising more interrupts than its driver drains halts the kernel; no trapframe is corrupted and no scheduler transition happens. Reachable only with a bound route (default-off witness today) | src/kernel/syscall_split.rs, src/arch/{x86_64/descriptor_tables.rs,aarch64/boot.rs,riscv64/boot.rs} | found during IRQ-AUTH1\n"
      "- 2026-11-10 | OPEN OBSERVATION — `remove_unpublished_endpoint_locked` refuses an endpoint removal when any IRQ route targets a NOTIFICATION slot whose index equals the ENDPOINT index (the two index spaces are compared). It can only refuse a removal, never corrupt a route; preserved unchanged by IRQ-AUTH1 | src/kernel/boot/spawn_ipc_cap_txn.rs | found during IRQ-AUTH1\n")

patch("doc/KERNEL_UNLOCKING.md", '''and `wip/notif-waiter1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''', '''and `wip/notif-waiter1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''' + record + '\n')
print("docs applied")
