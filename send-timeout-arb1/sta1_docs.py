#!/usr/bin/env python3
"""SEND-TIMEOUT-ARB1 documentation commit. Usage: sta1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

# ── doc/IPC.md: §8.12 cross-references, then the new §8.13 ─────────────────────────────────
patch("doc/IPC.md", '''generation (and, for the timeout, while the collected deadline is still armed and due).''',
      '''generation. (The timeout's deadline condition was also checked here until SEND-TIMEOUT-ARB1
moved it into the drain, before the withdrawal; see §8.13.)''')
patch("doc/IPC.md", '''* the timeout's order: claim at rank 2, then remove at rank 3. A timeout that claims after a
  receiver already took the waiter still ends the cycle with `TimedOut` although the message was
  delivered (recorded in `doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`, proposed SEND-TIMEOUT-ARB1).''',
      '''* the timeout's order: claim at rank 2, then remove at rank 3. A timeout that claimed after a
  receiver already took the waiter still ended the cycle with `TimedOut` although the message was
  delivered. SEND-TIMEOUT-ARB1 reversed this order (§8.13).''')

IPC813 = '''### 8.13 Blocked-send arbitration (SEND-TIMEOUT-ARB1)

A blocking-send cycle's message travels inside its `SenderWaiter`, so **the waiter is the cycle's
arbitration token**. The one irrevocable step of a cycle is the rank-3 removal of its exact waiter.
Whoever performs that removal owns the cycle: its ending, and every obligation its message still
carries (delivery, or cancelling a transfer envelope and its pin).

| Remover | Then |
|---|---|
| a receiver's take (consume, or refill into a freed buffered slot) | delivers the message, then claims `Consumed` at rank 2 and wakes only a claimed cycle (§8.12) |
| the send-timeout drain (`remove_sender_waiter_for_cycle_locked`) | claims `TimedOut` at rank 2, settles the envelope the withdrawn message carried, and wakes only if it claimed |
| `destroy_endpoint` | settles each removed waiter's envelope, then ends each cycle with the `WrongObject` code |
| nobody: the broad block could not publish its waiter (Phase C refused) | no waiter will ever exist, so the blocker ends its own cycle with the refusal's error code, wakes the sender and dispatches |

Death ends a cycle without a claim (the exit sweep removes the task's waiters by TID, §8.12).

**The timeout drain** (`SharedKernel::drain_send_timeout_post_work`) runs these steps:

1. **Not due:** `now < deadline` is refused before any lock.
2. **Precheck, rank 2, read-only:** the exact `{tid, asid, send_generation}` must still be
   `Blocked(EndpointSend)`. Stale work is refused here with no mutation.
3. **Withdraw, rank 3:** remove the exact waiter. This is the arbitration point. If no waiter
   matches (a receiver already took it, it is not yet published, or the cycle is over), nothing
   is mutated and the deadline stays armed.
4. **Claim, rank 2:** this is uncontested, because nothing else can end a cycle whose waiter this
   drain holds. The claim no longer checks the deadline; the drain checked it in step 1.
5. **Settle** the withdrawn message's envelope, once, after rank 3 is released.
6. **Wake** at rank 1, only if step 4 claimed.

The claim in step 4 fails only if the sender stopped being blocked in this cycle between steps 3
and 4. That needs a death, and a death ends the cycle without a claim. The drain then still settles
the envelope and wakes nobody, so nothing is stranded or leaked.

**The pre-publication window.** On the broad in-lock route the sender is made
`Blocked(EndpointSend)` with its deadline armed (Phase B) before its waiter is published (Phase C).
On SMP this is the production route, because the off-lock publication is refused when more than
one CPU dispatches. A timeout in that window finds no waiter and changes nothing. The waiter is
then published for a live cycle, and a later timer pass (or a receiver) ends that cycle. No waiter
ever outlives its cycle.

**The parked envelope.** The in-lock route now returns `BlockingSendProducerOutcome::BlockedInLock`
when it parks the sender. The `IpcSend` syscall still reports `WouldBlock`, and the legacy route
maps the variant to `Err(WouldBlock)`. What changed is that the syscall no longer releases the
transfer envelope: the parked waiter owns it. Before this change, a parked capability send's
waiter named an envelope the syscall had already given back.

**The result always agrees with the message.**

* A sender sees `OK` exactly when a receiver obtained (or was refilled with) its message.
* It sees `TimedOut` exactly when no receiver ever can.
* It sees an error when its endpoint was destroyed or its waiter could not be published.

There is no wall-clock tie-break. At or after the deadline the winner is whichever of the take and
the withdrawal reaches the waiter first.

**Not changed:**

* receiver arbitration and the endpoint queue;
* the public error codes and message layouts;
* the locks, ranks and capacities (no acquisition is nested);
* notifications and IRQs;
* the endpoint-removal index-space observation.

**Coverage.**

* `kernel::boot::tests::ipc_sender_id1::send_timeout_arb1`:
  * R1 (all three receiver seams) and R4 (refill): a timeout inside the receiver's take→claim
    window;
  * R2: a receiver inside the drain's withdraw→claim window;
  * R3: a timeout in the Phase B→C window;
  * R5: a parked capability send through the real `IpcSend` syscall;
  * R6: the deadline boundary;
  * R7: stale work of an older cycle;
  * R8: a dead sender;
  * a refused publication;
  * endpoint destruction.
* `...::shared_region::send_timeout_arb1_envelope`:
  * R5b: the envelope and pin after a timeout lands on a taken waiter;
  * R9: a failed claim after the withdrawal.

The windows a production function does not return across are entered through test-only phase
hooks (`sender_waiter::send_arb_phase_hook`, `cfg(test)`):

* between the broad block's Phase B and Phase C;
* between the drain's withdrawal and its claim.

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC813)

# ── doc/AI_AGENT_RULES.md ───────────────────────────────────────────────────────────────────
patch("doc/AI_AGENT_RULES.md", '''  through `sender_waiter` (one cycle by its triple; a task by its TID, which names one TCB).
''', '''  through `sender_waiter` (one cycle by its triple; a task by its TID, which names one TCB).
- The sender waiter is the blocking-send cycle's arbitration token (SEND-TIMEOUT-ARB1,
  `doc/IPC.md` §8.13). Whoever removes it at rank 3 owns the cycle's ending and its message's
  envelope. A timeout withdraws the exact waiter BEFORE it claims; finding no waiter, it changes
  nothing. Any new path that removes a sender waiter must end that cycle itself. Never claim a
  `TimedOut` ending without holding the withdrawn waiter. Never release the envelope of a send
  that parked a waiter.
''')

# ── doc/KERNEL_TEST_RULES.md ────────────────────────────────────────────────────────────────
patch("doc/KERNEL_TEST_RULES.md", '''> **IPC-SENDER-ID1 (2026-11-10).** A test of a blocked sender blocks it for real''',
      '''> **SEND-TIMEOUT-ARB1 (2026-11-10).** A window that a production function does not return
> across is entered through a `cfg(test)` phase hook at that exact point
> (`sender_waiter::send_arb_phase_hook`: the broad block's Phase B→C and the timeout drain's
> withdraw→claim), never by re-implementing the phase in the test. An arbitration test asserts
> that the sender's result agrees with what happened to the message: who received it, whether a
> waiter is left, the envelope's state and the wake count. The return code alone is not enough.
> See `doc/IPC.md` §8.13.

> **IPC-SENDER-ID1 (2026-11-10).** A test of a blocked sender blocks it for real''')

# ── doc/KERNEL_LOCKING.md (both IPC-SENDER-ID1 notes) ───────────────────────────────────────
ISI1_NOTE_END = '''> follows only a successful claim. No acquisition, lock or ordering was added; a refused claim now
> takes one fewer (no wake). See `doc/IPC.md` §8.12.'''
patch("doc/KERNEL_LOCKING.md", ISI1_NOTE_END, ISI1_NOTE_END + '''

> **SEND-TIMEOUT-ARB1 (2026-11-10).** The send-timeout drain's domain claims are now: a read-only
> rank-2 precheck, the rank-3 withdrawal of the exact waiter, the rank-2 claim, the rank-3 then
> rank-6 settle, and the rank-1 wake. They are sequential and never nested; before this change the
> drain claimed at rank 2 first and removed at rank 3 second. Two seams gain a claim and a wake,
> each after its rank-3 section is released:
> * `destroy_endpoint` ends each cycle it removes;
> * a refused Phase C publication ends its own cycle.
> No lock or rank was added. See `doc/IPC.md` §8.13.''', count=2)

# ── doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md ───────────────────────────────────────────────────
D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
OLD_CONF = "- 2026-11-10 | CONFIRMED, NOT REPAIRED (proposed SEND-TIMEOUT-ARB1) — the send timeout claims"
s = (wt / D).read_text()
start = s.index(OLD_CONF); end = s.index("\n", start) + 1
conf_line = s[start:end]
assert conf_line.rstrip().endswith("| found during IPC-SENDER-ID1")
new_conf = conf_line.replace("CONFIRMED, NOT REPAIRED (proposed SEND-TIMEOUT-ARB1)", "RESOLVED (SEND-TIMEOUT-ARB1)", 1)
new_conf = new_conf.replace(
    "| src/runtime.rs (drain_send_timeout_post_work), src/kernel/boot/ipc_state.rs | found during IPC-SENDER-ID1\n",
    "Resolution: the waiter is the arbitration token. The drain withdraws the exact waiter at rank 3 before it claims at rank 2; with no waiter it changes nothing and leaves the deadline armed, so the Phase B→C window ends in a published waiter for a live cycle (`doc/IPC.md` §8.13). Confirmed by R1–R4, R6 and R5b failing on `414afb86` | src/runtime.rs (drain_send_timeout_post_work), src/kernel/boot/sender_waiter.rs | found during IPC-SENDER-ID1; resolved in SEND-TIMEOUT-ARB1\n")
assert new_conf != conf_line and "resolved in SEND-TIMEOUT-ARB1" in new_conf
s = s[:start] + new_conf + s[end:]
(wt / D).write_text(s)
patch(D, "- 2026-11-10 | OPEN OBSERVATION — `destroy_endpoint` removes the parked senders and settles their envelopes but does not wake them, leaving them `Blocked(EndpointSend)` until a deadline (if any) fires. Its only production callers are the default-off oracle provisioning rollbacks, which run before any sender blocks | src/kernel/boot/ipc_state.rs | found during IPC-SENDER-ID1\n",
      "- 2026-11-10 | RESOLVED (SEND-TIMEOUT-ARB1; inseparable from the arbitration contract) — `destroy_endpoint` removes the parked senders and settles their envelopes but does not wake them, leaving them `Blocked(EndpointSend)` until a deadline (if any) fires. Its only production callers are the default-off oracle provisioning rollbacks, which run before any sender blocks. Why it is inseparable: once a waiter's removal is the arbitration point, a timeout that finds no waiter leaves the cycle alone, so a destroyed endpoint's sender would never be woken, not even by its deadline. Resolution: every removed waiter is kept, and after the settle each cycle is ended with the `WrongObject` code (a stale record is refused by the claim). Guarded by `send_timeout_arb1::endpoint_destruction_ends_the_cycles_it_removes`, which fails on `414afb86` | src/kernel/boot/ipc_state.rs | found during IPC-SENDER-ID1; resolved in SEND-TIMEOUT-ARB1\n"
      "- 2026-11-10 | RESOLVED (SEND-TIMEOUT-ARB1) — the in-lock blocking-send route (the production route whenever more than one CPU dispatches, and under any hosted trap) returned `Err(WouldBlock)` after parking the sender, so the `IpcSend` syscall's error path released the transfer envelope that the parked waiter's message still named. A receiver then delivered a dead envelope handle, and a timeout found nothing to cancel. Resolution: the route returns `BlockingSendProducerOutcome::BlockedInLock`; the syscall still reports `WouldBlock` but leaves the envelope to the cycle's winner; the legacy route maps it to `Err(WouldBlock)`; the ABI is unchanged. Confirmed by R5 failing on `414afb86` | src/kernel/boot/ipc_state.rs, src/kernel/syscall/ipc.rs | found and resolved in SEND-TIMEOUT-ARB1\n"
      "- 2026-11-10 | RESOLVED (SEND-TIMEOUT-ARB1) — the broad block's Phase C (`enqueue_sender_waiter`) can refuse (a full waiter queue) after Phase B made the sender `Blocked(EndpointSend)`. The `?` returned the error with the task still blocked and no waiter, so nothing could ever end the cycle except its deadline, and with the arbitration contract not even that. Resolution: the blocker owns a cycle it could not publish; it ends the cycle with the refusal's error code (`SyscallError::from(err).code()`), wakes the sender and dispatches, then returns the error. Confirmed by `send_timeout_arb1::a_refused_publication_never_strands_the_blocked_sender` failing on `414afb86` | src/kernel/boot/ipc_state.rs | found and resolved in SEND-TIMEOUT-ARB1\n")

# ── doc/KERNEL_UNLOCKING.md: the stage record ───────────────────────────────────────────────
patch("doc/KERNEL_UNLOCKING.md", '''Verdict: **IPC_SENDER_ID1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/ipc-sender-id1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''', '''Verdict: **IPC_SENDER_ID1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/ipc-sender-id1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''' + record + '\n')
print("docs applied")
