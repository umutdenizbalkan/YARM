#!/usr/bin/env python3
"""IPC-SENDER-ID1 documentation commit. Usage: isi1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

IPC812 = '''### 8.12 Endpoint sender-waiter identity (IPC-SENDER-ID1)

A sender parked on a full or synchronous endpoint is a `SenderWaiter` carrying one
**blocking-send cycle**, `{tid, asid, send_generation}` (`SenderWaiter::wake_target`).

**Ending a cycle.** There is one rank-2 claim, `sender_waiter::claim_blocked_send_cycle_locked`,
and both ways a cycle can end make it:

* a receiver that consumed the message, through each of the three receiver-side seams
  (`apply_blocked_sender_wake`, `apply_split_sender_wake_plan`,
  `apply_split_sender_wake_plan_split`);
* the send-timeout drain (`drain_send_timeout_post_work`).

The claim succeeds only while that exact incarnation is `Blocked(EndpointSend)` with that
generation (and, for the timeout, while the collected deadline is still armed and due). In the
same acquisition it parks the cycle's completion (success or `TimedOut`), makes the sender
`Runnable` and disarms its deadline. Whoever claims first ends the cycle. Every later claimant is
refused with no mutation.

Only a successful claim is followed by the wake. A refused claim wakes nothing, publishes nothing
and is not an error, so a receive that already holds the message never fails because of it. A
target is therefore never applied to another cycle: neither a newer blocking send of the same
task nor another task. An ASID-less sender (a kernel sender) is the cycle with `asid: None`. It is
claimed only by a TCB whose ASID is also `None`, and is woken without a completion.

**Removing waiters.**

* *One cycle* (the timeout): `remove_sender_waiter_for_cycle_locked` matches the whole triple.
* *A task* (exit, death and reap: `clear_ipc_waiters_for_tid` and
  `reap_claim::clear_ipc_waiters_for_identity_locked`): `remove_sender_waiters_for_task_locked`
  matches the numeric TID, by proof rather than by convenience:
  * a TID names one TCB for the life of the system;
  * registration of an existing TID is a no-op, and the dynamic allocator never returns a TID
    a TCB still carries;
  * no production path removes a TCB that went live from the table (the one slot-clearing
    owner, `spawn_reservation::clear_reservation_slot`, only clears a reservation or an undone
    thread spawn that never ran).

  Every waiter carrying the TID therefore belongs to the TCB being torn down, whatever ASID or
  generation it recorded, and removing all of them is the lifecycle contract. Matching a stored
  ASID instead would leave such a waiter with nobody to remove it.

Both removals compact the queue, as a receiver's take does, so a later sender never overtakes an
earlier one. A removed waiter that still owns a transfer envelope is settled once, after rank 3 is
released, by whoever removed it. A waiter a receiver took is the receiver's, and no later removal
finds it.

**Not changed.** These stay as they were:

* receiver arbitration, the endpoint queue and the ABI;
* the timeout's order: claim at rank 2, then remove at rank 3. A timeout that claims after a
  receiver already took the waiter still ends the cycle with `TimedOut` although the message was
  delivered (recorded in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`, proposed SEND-TIMEOUT-ARB1).

**Coverage.**

* `kernel::boot::tests::ipc_sender_id1`, which uses real blocking sends, the production take and
  timeout pipeline, and all three seams, in two-CPU orders. It covers:
  * S1: a TID is never recycled;
  * S2: a stale wake against a newer cycle;
  * S3: timeout against receive;
  * S4: a dead sender;
  * S6: repeated cycles;
  * arrival order;
  * the ASID-less cycle;
  * the claim and both removals at the owner.
* `...::shared_region::ipc_sender_id1_envelope` (S5): envelope and pin ownership.

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC812)

patch("doc/AI_AGENT_RULES.md", '''  exactly that bit. Every delivery ending is completed exactly once through
  `InterruptCompletion::complete`. Never widen this into suppressing trap errors.
''', '''  exactly that bit. Every delivery ending is completed exactly once through
  `InterruptCompletion::complete`. Never widen this into suppressing trap errors.
- A blocking-send cycle ends by ONE claim (IPC-SENDER-ID1, `doc/IPC.md` §8.12). Never wake a
  blocked sender by bare TID: a receiver-side or timeout-side path calls
  `sender_waiter::claim_blocked_send_cycle_locked` with the waiter's exact
  `{tid, asid, send_generation}` and wakes only if that claim succeeded. Remove sender waiters only
  through `sender_waiter` (one cycle by its triple; a task by its TID, which names one TCB).
''')

patch("doc/KERNEL_TEST_RULES.md", '''> **IRQ-DELIVERY1 (2026-11-10).** A full Notification queue coalesces.''', '''> **IPC-SENDER-ID1 (2026-11-10).** A test of a blocked sender blocks it for real
> (`ipc_send_with_deadline` by the current task, or the off-lock publish+commit) and gives it an
> ASID when it stands for a user task. A two-CPU race is written as the production phases in the
> order two CPUs can run them (the take, the timeout collect+drain, the wake seam), not by
> injecting the post-race state. A sender-wake fixture's "sender" must be blocked in the send
> cycle its target names; a receive-blocked task is never a sender. See `doc/IPC.md` §8.12.

> **IRQ-DELIVERY1 (2026-11-10).** A full Notification queue coalesces.''')

LOCK_NOTE = '''

> **IPC-SENDER-ID1 (2026-11-10).** The receiver-side sender publication is now the same rank-2
> claim the send-timeout drain makes (`sender_waiter::claim_blocked_send_cycle_locked`): it parks
> the completion and makes the sender `Runnable` in that one task acquisition. The rank-1 wake
> follows only a successful claim. No acquisition, lock or ordering was added; a refused claim now
> takes one fewer (no wake). See `doc/IPC.md` §8.12.'''
patch("doc/KERNEL_LOCKING.md", "clears exactly one bit inside its own. No acquisition, lock or ordering was added. See\n> `doc/IPC.md` §8.11.",
      "clears exactly one bit inside its own. No acquisition, lock or ordering was added. See\n> `doc/IPC.md` §8.11." + LOCK_NOTE, count=2)

D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
patch(D, "- 2026-11-10 | OPEN OBSERVATION — the endpoint SENDER waiter sweeps beside the converted clears (task teardown, broad timeout scan, reap/exit claim body) still match the bare numeric TID. They are not receive waiters and were out of NOTIF-WAITER1's scope; whether a recycled TID can remove an unrelated sender waiter is not established | src/kernel/boot/ipc_state.rs, src/kernel/boot/reap_claim.rs | found during NOTIF-WAITER1\n",
      "- 2026-11-10 | VERIFIED, NO DEFECT (IPC-SENDER-ID1) — the endpoint SENDER waiter sweeps beside the converted clears (task teardown, broad timeout scan, reap/exit claim body) still match the bare numeric TID. They are not receive waiters and were out of NOTIF-WAITER1's scope; whether a recycled TID can remove an unrelated sender waiter is not established. Resolution: a TID names one TCB for the life of the system (registration of an existing TID is a no-op, the allocator never returns a TID a TCB carries, and only `spawn_reservation::clear_reservation_slot` removes a TCB, never one that ran), so the numeric TID is the exact task-incarnation predicate for exit, death and reap; both sweeps now call `sender_waiter::remove_sender_waiters_for_task_locked`. The broad timeout scan's sweep is unreachable by construction (its only `expired` write follows an unconditional `continue`). Guarded by `ipc_sender_id1::s1_a_tid_is_never_recycled_so_task_scoped_removal_is_exact` (`doc/IPC.md` §8.12) | src/kernel/boot/sender_waiter.rs | found during NOTIF-WAITER1; verified in IPC-SENDER-ID1\n"
      "- 2026-11-10 | RESOLVED (IPC-SENDER-ID1) — the receiver-side sender wake published the cycle's completion by exact `{tid, asid, send_generation}` but then woke the bare TID unconditionally, and the publication left the sender `Blocked`. With a send timeout settled on another CPU inside a receiver's take→wake window: a stale wake woke the sender's NEXT blocking send (no completion, deadline disarmed, its waiter stranded), a timeout overwrote a receiver's published success with `TimedOut`, and the broad receive returned `WouldBlock` after taking the message (dropping it and any envelope). Also an ASID-bearing target woke an ASID-less sender, and timeout/exit removals left holes that let a later sender overtake earlier ones. Resolution: one rank-2 claim for both endings, wake only after a successful claim, exact-cycle and task-scoped removals that compact (`doc/IPC.md` §8.12). Confirmed by 9 reproducers failing on `16e5c04` | src/kernel/boot/sender_waiter.rs, src/kernel/boot/ipc_state.rs, src/runtime.rs | found and resolved in IPC-SENDER-ID1\n"
      "- 2026-11-10 | CONFIRMED, NOT REPAIRED (proposed SEND-TIMEOUT-ARB1) — the send timeout claims a cycle at rank 2 and only then removes its waiter at rank 3, while a receiver takes the waiter (and the message) at rank 3 and claims at rank 2. A timeout that claims inside a receiver's take→claim window, on another CPU, ends the cycle `TimedOut` although the receiver delivers the message, so a sender that retries on `TimedOut` sends it twice. The same holds for the broad block's Phase B→C window (a timeout between the `Blocked` write and the waiter's publication leaves a waiter of a `Runnable` sender, whose message a later receiver delivers). Since IPC-SENDER-ID1 neither case wakes, publishes or settles twice; only the delivered-versus-`TimedOut` disagreement remains. Repair needs the waiter removal to be the arbitration point (timeout removes the exact waiter first, then claims) — a timeout-arbitration change outside IPC-SENDER-ID1's scope. Requires two CPUs concurrently in kernel; reachable through the IpcSend timeout argument | src/runtime.rs (drain_send_timeout_post_work), src/kernel/boot/ipc_state.rs | found during IPC-SENDER-ID1\n"
      "- 2026-11-10 | OPEN OBSERVATION — `destroy_endpoint` removes the parked senders and settles their envelopes but does not wake them, leaving them `Blocked(EndpointSend)` until a deadline (if any) fires. Its only production callers are the default-off oracle provisioning rollbacks, which run before any sender blocks | src/kernel/boot/ipc_state.rs | found during IPC-SENDER-ID1\n")

patch("doc/KERNEL_UNLOCKING.md", '''Verdict: **IRQ_DELIVERY1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-delivery1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''', '''Verdict: **IRQ_DELIVERY1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-delivery1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''' + record + '\n')
print("docs applied")
