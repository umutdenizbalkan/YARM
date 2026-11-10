#!/usr/bin/env python3
"""NOTIF-WAITER1 documentation commit. Usage: nw1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

IPC89 = '''### 8.9 Notification waiter (NOTIF-WAITER1)

**Contract.** A notification incarnation has at most one blocking receiver.
`IpcSubsystem::notification_waiters[i]` names it as `{tid, asid, wait_generation}`. Every
decision about the slot is taken at IPC rank 3 by `src/kernel/boot/notification_waiter.rs`, in
one critical section that also sees the incarnation's generation and its event queue:

* **immediate take.** The capability's generation is compared in the same acquisition as the
  take, so an older capability never consumes a newer incarnation's event.
* **publication.** It happens after the receiver is `Blocked(EndpointReceive)` and its receive
  wait generation has been bumped, so a signal that finds the record can always wake it. In
  order:
  1. a gone or recycled incarnation refuses (`StaleCapability`);
  2. a queued event is consumed for the receiver, because the signal raced the block;
  3. another receiver's record refuses (`EndpointQueueFull`), and so does the same receiver's
     record unless the new block has a strictly newer wait generation;
  4. otherwise the record is published.
* **stale reclaim.** A record is live only while its exact `{tid, asid}` task is
  `Blocked(EndpointReceive)` under the record's wait generation. A competing record is tested
  at task rank 2, before rank 3 is re-entered. If it is stale, the decision is retaken and may
  replace exactly that record. The wait generation only grows, so a record that is once stale
  stays stale. A record left behind by any wake therefore never locks out the next receiver.
* **removal.** Each removal matches the complete `{tid, asid}`, never a bare TID. This covers
  the signal's take, destruction (NOTIF-LIFETIME1, §8.8), task teardown, the broad timeout scan,
  the reap and exit claim body, and the split receive-timeout settle. A record of a different
  incarnation that shares the numeric TID is left alone; the signal's exact proof never wakes
  it, and the next receive reclaims it.

**No lost event.** Either the receiver consumes an available event, or it is the published,
wakeable waiter. The signal side is unchanged: the broad route and the split IRQ delivery queue
the event and take the record in one rank-3 acquisition, then wake through the exact-identity
proof. So:

* a signal before publication is found by step 2;
* a signal after publication finds the record.

**Restoration.** Every unpublished outcome (gone incarnation, event taken, competing receiver)
restores the receiver through the receive family's unwind: `wake_tid_to_runnable`, which
disarms its timeout and enqueues it, then `dispatch_next_task`. This is the IPC-DISPLACE1
refusal shape. A published receiver that a timeout or teardown settled in the window between
the block and the publication is no longer parked. Its exact record is retracted, so the
timeout's wake stands and nothing stale remains.

**Lifetime.** A waiter holds no reference. The final capability release still destroys the
object, retires the waiter and wakes it (§8.8).

**Reachability.** No syscall reaches the blocking notification receive on any ISA or build:

* NR 2 and NR 5 validate an endpoint capability before any receive (`validate_endpoint_right`
  answers `WrongObject` for a notification);
* the witness builds' only notification door is the non-blocking NR 5 probe, which publishes no
  waiter.

This is a kernel primitive repair, proven hosted. The live UART/PL011 witnesses exercise the
unchanged signal delivery.

**Coverage.** `kernel::boot::tests::notif_waiter1`:

* N1–N3, the defects, which fail on `dc5c8fd`;
* first-receiver survival, exactly-once delivery and restoration of the refused receiver;
* wait-generation supersession and stale reclaim;
* signals before and after publication, broad and split (the split delivery entered for CPU 1);
* production timeouts in the window and after publication;
* exit, death and destruction during a wait, and the new incarnation left untouched;
* recycled-identity teardown and repeated signals;
* reference accounting and the generation-checked take;
* a structural guard that only the owner publishes and no removal matches a bare TID.

The handshakes run at a test-only hook between the block and the publication. Multi-receiver
FIFO queues are out of scope.

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC89)

patch("doc/AI_AGENT_RULES.md", '''  before a capability becomes visible and release it after the capability is gone, through
  `notification_lifetime`.
''', '''  before a capability becomes visible and release it after the capability is gone, through
  `notification_lifetime`.
- A notification has one blocking receiver (NOTIF-WAITER1, `doc/IPC.md` §8.9). Never write
  `notification_waiters` outside `notification_waiter`: publish only after the block, through
  the one rank-3 decision (generation, queue, competing record), and remove only by complete
  `{tid, asid}`. A refused receiver is restored through the receive family's unwind, never by
  a status write.
''')

patch("doc/KERNEL_TEST_RULES.md", '''> **Superseded by NOTIF-LIFETIME1 (2026-11-09).**''', '''> **NOTIF-WAITER1 (2026-11-10).** A notification waiter test publishes its record through the
> real blocking receive (`ipc_recv` / `ipc_recv_with_deadline` on a RECEIVE cap), not by writing
> `notification_waiters`, so the record names the task's true `{tid, asid}`. A window between
> the block and the publication is driven with `notif_recv_phase_hook`, never by reordering
> production code. See `doc/IPC.md` §8.9.

> **Superseded by NOTIF-LIFETIME1 (2026-11-09).**''')

patch("doc/KERNEL_UNLOCK_AUDIT.md", '''| `wake_tid_to_runnable_split` | `complete_blocked_waiter_delivery_split` | 1 |''', '''| `wake_tid_to_runnable_split` | `complete_blocked_waiter_delivery_split` | 1 |

> NOTIF-WAITER1: `notification_recv_blocking`, the notification receive's own owner moved out
> of `ipc_recv_with_optional_deadline`, is a further direct caller of `wake_tid_to_runnable`:
> the unpublished unwind (gone incarnation, event taken, competing receiver). The live census
> is `stage199d_wa2b_wake_owner_census`.''')
patch("doc/KERNEL_UNLOCK_AUDIT.md", "The producer (`ipc_recv_with_optional_deadline`'s notification arm) bumps",
      "The producer (`notification_recv_blocking`, formerly `ipc_recv_with_optional_deadline`'s notification arm; NOTIF-WAITER1, `doc/IPC.md` §8.9) bumps")
patch("doc/KERNEL_UNLOCK_AUDIT.md", "| rollback, not delivery | `wake_tid_to_runnable` (via `recv_block_unwind_race`), `direct_dispatch_rollback_split` |",
      "| rollback, not delivery | `wake_tid_to_runnable` (via `recv_block_unwind_race`, and via `notification_recv_blocking`'s unpublished unwind), `direct_dispatch_rollback_split` |")

patch("doc/KERNEL_UNLOCKING.md", '''`main` `b81e0317` and every other ref unchanged.


## U6-FRAME''', '''`main` `b81e0317` and every other ref unchanged.

''' + record + '''

## U6-FRAME''')

patch("doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md", "| src/kernel/boot/notification_lifetime.rs | resolved in NOTIF-LIFETIME1\n",
      "| src/kernel/boot/notification_lifetime.rs | resolved in NOTIF-LIFETIME1\n"
      "- 2026-11-10 | RESOLVED (NOTIF-WAITER1) — notification waiter overwrite (N1), signal lost between the empty check and publication (N2), an old receiver publishing onto a reused slot (N3), a stale record left by a timeout in the window, and bare-TID notification-waiter removal. All confirmed in the kernel primitive; none reachable by a syscall today (`doc/IPC.md` §8.9) | src/kernel/boot/notification_waiter.rs | resolved in NOTIF-WAITER1\n"
      "- 2026-11-10 | OPEN OBSERVATION — the endpoint SENDER waiter sweeps beside the converted clears (task teardown, broad timeout scan, reap/exit claim body) still match the bare numeric TID. They are not receive waiters and were out of NOTIF-WAITER1's scope; whether a recycled TID can remove an unrelated sender waiter is not established | src/kernel/boot/ipc_state.rs, src/kernel/boot/reap_claim.rs | found during NOTIF-WAITER1\n")
print("docs applied")
