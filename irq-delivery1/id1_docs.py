#!/usr/bin/env python3
"""IRQ-DELIVERY1 documentation commit. Usage: id1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))

IPC811 = '''### 8.11 IRQ delivery saturation (IRQ-DELIVERY1)

**Contract.** An authorized interrupt delivered to a live routed Notification always ends in one
of these:

* a queued event;
* a retained pending indication for its line;
* a settled refusal that already existed: the route is gone, or the target incarnation is gone.

None of them is an error, and none reaches the trap as one. There is no ending in which the
queue is full, the interrupt is acknowledged, and the event is forgotten.

**State.** A Notification keeps its bounded `irq_queue` (`max_depth` events in arrival order).
It also keeps `overflow_pending`, one bit per IRQ line (`MAX_IRQ_LINES <= 64`, checked at compile
time).

* `send_irq(line)` queues only while no bit is set and the queue has room.
* Otherwise it sets the line's bit and reports `IrqSignal::Coalesced { newly_pending }`.
  `newly_pending` is false when the line was already pending.
* While any bit is set the object stays in coalescing mode. A busy line cannot refill the queue
  and starve a line that overflowed.
* Its only remaining `Err` is a line no route can carry (an internal invariant).

**Receive.** A receive takes the oldest queued event first. With the queue empty, it takes the
lowest pending line and clears exactly that bit. The message is the same either way: the line,
in the header and the payload, so the ABI is unchanged.

A pending line means "at least one interrupt on this line since its last event". Coalesced
interrupts are counted by nobody: a driver that needs a count reads its device.

**Ordering.** Every signal and receive runs inside the IPC rank-3 critical section that owns the
object, so they are totally ordered. A signal for line X after the receive that cleared X's bit
sets it again (or queues). An earlier consumption cannot erase what a later signal published.

The state belongs to the incarnation:

* destruction removes the object with its queue and its bits;
* a replacement starts empty;
* the route generation check (§8.10) keeps an old route from delivering into it.

**Outcomes.**

* The split delivery (`deliver_external_irq_split`) reports `IrqDeliveryOutcome::Coalesced
  { notification_idx, generation, newly_pending }`, marker `coalesced`, with no propagated
  error. The bridge settles it as success.
* The broad delivery (`signal_notification`) returns `Ok`.
* `Failed` now means only an invariant failure.
* No other error class changed meaning: `TrapHandleError` is not suppressed and no other
  `KernelError` became success.
* A saturated interrupt does not touch the trapframe, the PC, the return value or the
  scheduler placement.

**Completion.** The single completion owner (`InterruptCompletion::complete`) runs exactly once for
every ending, saturated or not:

* x86_64: the LAPIC EOI;
* AArch64: the claimed INTID's GICC_EOIR, still written by the vector tail;
* RISC-V: the PLIC completion paired with its claim.

The pending indication is kernel state, not controller state. Nothing is left unacknowledged
for the device to re-raise.

**Not changed.** These are out of scope:

* no syscall, capability type, rights bit, lock, or unbounded storage;
* no driver masking or acknowledgement protocol;
* no binding API.

A level-triggered device that keeps its line asserted re-raises after every completion whether
or not the queue is full. That is driver work (device acknowledgement or masking), recorded in
`doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`.

**Coverage.**

* `kernel::boot::tests::irq_delivery1`:
  * Q1–Q7: nonfatal for user and idle origin, bridge, ordinary delivery unchanged, two
    lines, destruction;
  * exact completion on both tokens;
  * interleavings I2–I8 at the rank-3 linearization point;
  * starvation;
  * both producers;
  * the object owner.
* The default-off `irq-saturation-witness` drives real device interrupts past a full queue on
  all three interrupt controllers, through `scripts/qemu-irq-saturation-witness-smoke.sh` and
  `scripts/grade-irq-saturation-witness.py`.

---

## 9. Authoring rule'''
patch("doc/IPC.md", "---\n\n## 9. Authoring rule", IPC811)
patch("doc/IPC.md", '''**Not changed.** A full notification queue still propagates `EndpointQueueFull` out of the
interrupt. The port then halts after acknowledging it. This is recorded for IRQ-DELIVERY1
(`doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md`).''', '''**Not changed.** A full notification queue still propagated `EndpointQueueFull` out of the
interrupt, and the port then halted after acknowledging it. IRQ-DELIVERY1 resolved this (§8.11).''')

patch("doc/AI_AGENT_RULES.md", '''  is never redirected. A route is owned by its exact IRQ capability and released through the
  removal report, so a new removal path must record what it removes in that report.
''', '''  is never redirected. A route is owned by its exact IRQ capability and released through the
  removal report, so a new removal path must record what it removes in that report.
- IRQ saturation is a settled ending (IRQ-DELIVERY1, `doc/IPC.md` §8.11). Never make a full
  Notification queue an error, and never drop an interrupt to fit the queue. `send_irq` coalesces
  into the line's pending bit, a receive takes the queue, then the lowest pending line, and clears
  exactly that bit. Every delivery ending is completed exactly once through
  `InterruptCompletion::complete`. Never widen this into suppressing trap errors.
''')

patch("doc/KERNEL_TEST_RULES.md", '''> **IRQ-AUTH1 (2026-11-10).** A test binds an IRQ line with line authority''', '''> **IRQ-DELIVERY1 (2026-11-10).** A full Notification queue coalesces. A test that needs it
> saturates with real `send_irq` or real deliveries, then drains with real receives. Assert the
> received lines, not only the outcome. Completion is counted with
> `crate::arch::hal_adapters::completion_count_for_test::take()`, which counts the one completion
> owner (hosted builds have no controller to read back). See `doc/IPC.md` §8.11.

> **IRQ-AUTH1 (2026-11-10).** A test binds an IRQ line with line authority''')

LOCK_NOTE = '''

> **IRQ-DELIVERY1 (2026-11-10).** A full queue no longer fails the signalling acquisition.
> `send_irq` sets the line's pending bit inside the same rank-3 critical section, and a receive
> clears exactly one bit inside its own. No acquisition, lock or ordering was added. See
> `doc/IPC.md` §8.11.'''
patch("doc/KERNEL_LOCKING.md", "in its signalling acquisition. See `doc/IPC.md` §8.10.",
      "in its signalling acquisition. See `doc/IPC.md` §8.10." + LOCK_NOTE, count=2)

D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
patch(D, "- 2026-11-10 | CONFIRMED, NOT REPAIRED (proposed IRQ-DELIVERY1) — a full Notification IRQ queue",
      "- 2026-11-10 | RESOLVED (IRQ-DELIVERY1; confirmed during IRQ-AUTH1) — a full Notification IRQ queue")
patch(D, "Reachable only with a bound route (default-off witness today) | src/kernel/syscall_split.rs, src/arch/{x86_64/descriptor_tables.rs,aarch64/boot.rs,riscv64/boot.rs} | found during IRQ-AUTH1\n",
      "Reachable only with a bound route (default-off witness today). Resolution: a full queue sets the line's pending bit (`overflow_pending`) and coalesces repeats; a receive takes the queue, then the lowest pending line, clearing exactly that bit; the split outcome is `Coalesced` and the broad form returns `Ok`; the one completion still runs exactly once (`doc/IPC.md` §8.11). Confirmed with Q1–Q4 and Q6 failing on `c703226` and, live, a fatal halt after 16 deliveries on all three ports | src/kernel/boot/defs.rs, src/runtime.rs, src/kernel/boot/ipc_state.rs | found during IRQ-AUTH1; resolved in IRQ-DELIVERY1\n"
      "- 2026-11-10 | OPEN LIMITATION — a level-triggered device that keeps its line asserted re-raises after every completion, saturated queue or not. IRQ-DELIVERY1 makes saturation lossless and nonfatal, but it does not mask or quiesce a source: that needs driver-level device acknowledgement or line masking, which no driver protocol defines yet. The UART/PL011 witnesses drain the device per interrupt | src/kernel/boot/defs.rs | found during IRQ-DELIVERY1\n"
      "- 2026-11-10 | OPEN OBSERVATION — the RISC-V UART witness fixture opens its interrupt gate at the idle loop only when nothing is runnable. With the supervisor polling every tick, a long saturation sequence stalled after 6–8 claims on both `c703226` and the candidate. The saturation items now open at any user-return window; the eight ordinary items are unchanged. This is a fixture limitation, not a kernel defect | crates/yarm-control-plane-servers (fixture), src/arch/riscv64 | found during IRQ-DELIVERY1\n")

patch("doc/KERNEL_UNLOCKING.md", '''Verdict: **IRQ_AUTH1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-auth1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''', '''Verdict: **IRQ_AUTH1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/irq-auth1` at the commit that adds this record; `main` `b81e0317` and every other ref
unchanged.

''' + record + '\n')
print("docs applied")
