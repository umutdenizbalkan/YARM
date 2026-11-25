
#### IPC-D2-PUBLISH1 (from qualified `4b7b71e0`) — HARD-STOP: a decision for the rule's owner

This pass was asked to resolve the SMP2 receive-publication finding left by VFS-CORRECT1C. The
finding is the `D2_PUBLISH_RACE_UNWIND` marker that the fd witness reached at SMP2.

The evidence does not show an implementation defect. It shows that a stop-ship rule outlived its
precondition. So this pass changes no kernel, gate or grader. It ends with this brief.
- The `d2_publish_race_unwinds` rule (`doc/AI_AGENT_RULES.md` §14.3, `doc/KERNEL_TEST_RULES.md`
  Stage 106 rule 1, this file §3 and §8) and every smoke gate stay in force.
- Dev stays at `4b7b71e0`.
- Evidence: `wip/ipc-d2-publish1-evidence`.

**Decision needed.** Keep the zero rule as it is, or replace it with the falsifiable rule
below. Until then, any boot in which a client on one CPU calls a server that is parking on
another CPU fails its core smoke on x86_64 and AArch64. RISC-V prints the failed check and
exits 0 outside `QEMU_SMOKE_STRICT=1`.

**The six events** (VFS-CORRECT1C run 3, `R_race_smp2_<port>`, tested code `76c74efb`, evidence
`35267905`; the boot logs' hashes match that branch's manifest). They were attributed line by line
by `race_events.py` (evidence branch):

| port | log line | receiver (CPU) | wait gen | sender (CPU) | raced record | pair | unwind | settle | delivered once | reply committed |
|---|---|---|---|---|---|---|---|---|---|---|
| x86_64 | 56556 | VFS 10002 (0) | 301 | fork child 10009 (1) | 0 / 149 | yes | exact | `queue_non_empty` / `restored` | yes | yes |
| x86_64 | 56928 | VFS 10002 (0) | 303 | fork child 10009 (1) | 0 / 150 | yes | exact | same | yes | yes |
| x86_64 | 57530 | VFS 10002 (0) | 306 | fork child 10009 (1) | 0 / 152 | yes | exact | same | yes | yes |
| AArch64 | 106359 | VFS 10002 (0) | 294 | fork child 10009 (1) | 0 / 149 | yes | exact | same | yes | yes |
| RISC-V | 130220 | VFS 10002 (0) | 266 | thread 10008 (1) | 1 / 139 | yes | exact | same | yes | yes |
| RISC-V | 142091 | VFS 10002 (0) | 289 | fork child 10009 (1) | 1 / 151 | yes | exact | same | yes | yes |

**What the logs establish.** The markers and their fields:
- **Receive and block.** VFS entered a split blocking receive on its endpoint 10
  (`IPC_RECV_AFTER_CAP_OK`) and blocked with a fresh wait generation (`SCHED_BLOCK`,
  `D2_RECV_WAITER_TASK_BLOCKED`).
- **Message visible.** A call from a task resuming on CPU 1 was queued on endpoint 10
  (`IPCCALL_QUEUED_SPLIT_OK`, record index and generation). The queued route is the one a sender
  takes when no receive waiter is published.
- **Race and unwind.** The race pair was logged for VFS. The unwind restored the exact incarnation
  (`D2_RECV_SPLIT_UNWIND_OK … via=exact … priority=exact`) and settled `reason=queue_non_empty
  outcome=restored`.
- **Delivery.** VFS then bound that record's reply capability, the only binding of that record in
  the boot (`IPC_RECV_REPLY_CAP_RESPONDER_BOUND`), and the reply committed
  (`IPCREPLY_DIRECT_TERMINAL_CLAIM … resolution=commit`).
- **No failures.** No `IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED` and no displacement appear.

**What is inferred from source.**
- **Ordering.** Lines from two CPUs appear in print order. In the AArch64 event the enqueue line
  prints before VFS's `SCHED_BLOCK`. So the claim that the enqueue landed after VFS's empty
  immediate take and before its publication comes from the source:
  - the blocking lane is entered only after the immediate take found nothing
    (`try_split_blocking_ipc_recv_into_frame`, step (5));
  - the publication's atomic recheck returned `QueueNonEmpty` (`publish_recv_waiter_locked`).
- **Why default boots never hit it.** In the default SMP2 core smokes (VFS-CORRECT1C run 3, the same
  kernel code as `4b7b71e0`) every split park is on
  CPU 0: x86_64 16,975, AArch64 144, RISC-V 14,029. The fd witness at SMP2 parks 7 on CPU 1 per
  port.

**Emitters and counter, attributed.**
- **Broad emitter.** `KernelState::recv_block_unwind_race` (`kernel/boot/ipc_state.rs`) logs
  `D2_PUBLISH_RACE_UNWIND endpoint=<index>` and is the only call of `note_d2_publish_race_unwind`,
  the counter's only increment. It runs inside one `&mut KernelState` borrow, where the race cannot
  occur. No blocking receive has reached the broad arms since U9-RX3.
- **Split emitter.** The split lane's `QueueNonEmpty` arm (`try_split_blocking_ipc_recv_into_frame`,
  `kernel/syscall_split.rs`) logs `D2_PUBLISH_RACE_UNWIND endpoint=split` and increments nothing.
- **The six events.** Every one has the split spelling, so the counter was 0 on every boot. It is
  never printed, and the gates see only the marker.

**History of the rule.**
- `6241deb7` (Stage 106): D2 live-wired. The rule reads "MUST be 0 until the SharedKernel seam
  split lands", because every phase ran in one serialized borrow. `KERNEL_TEST_RULES` says a
  non-zero value "indicates the publish recheck disagreed with the Phase-1 dequeue under the same
  global borrow".
- `f4ac9f4b` (D7-B): the unconditional smoke rejects.
- `e47c50eb` (Stage 111): phase split, Outcome B, "without calling" the SharedKernel seams.
- `1fbcfbeb` (Stage 115): the rank-3 seam added but not live-wired.
- **The split lands.** The seam split this rule waited for is the four `SharedKernel` phase twins
  and the pre-lock blocking-receive route:
  - **x86_64, U9-RX3 `65a144cd`.** Its record: "THE `QueueNonEmpty` RACE IS NOW REACHABLE … the
    pre-lock route removes that serialization". It added the forced-race test.
  - **AArch64, U9-RX4 `11d6ba4d`.**
  - **RISC-V, U9-RECV-FINAL `f24ebab2`.**
  - **Multi-CPU parking, U9-RECV-BLOCK1 `6775f43b`.** Authenticated admission let a blocking
    receive park on a boot with more than one dispatching CPU, which is what makes a cross-CPU
    sender possible. U9-RECV-BLOCK2b then removed the pre-park would-block read and made the
    publication the only place the lane learns the queue became non-empty.
- **The note that it had lapsed.** U9-RX3's note in `stage115_d2_race_unwind_marker_is_zero` says
  the count "stays 0 … only because no sender has yet won that window — not because it cannot".
- **MARKER1 `d8b107ba`.** It confined the marker to the genuine race and left the gates unchanged,
  as it had to: it was repairing attribution, not the rule.

So the precondition has been met on all three ports since 2026-09-15. **What the rule protects
today:**
- On the broad route, its original invariant still holds and is still worth zero.
- On the split route, it forbids the one legal outcome of the design. It is satisfied only while
  no cross-CPU sender wins the window, and it would equally be satisfied by a defect that hid the
  race.

**The legal interleaving** (receiver R on CPU 0, sender S on CPU 1, endpoint E):
1. R: immediate take, rank 3 → empty.
2. R: Phase A, rank 1 → R off `current`.
3. R: Phase B, rank 2 → `Blocked(EndpointReceive)`, wait generation g.
4. S: send, rank 3 → no published waiter, so the message is queued on E.
5. R: Phase C, rank 3 → atomic recheck sees the queue non-empty, publishes nothing, `QueueNonEmpty`.
6. R: exact unwind of (2)–(3), authenticated by (R, ASID, priority, g) → `Restored`.
7. R: immediate take → S's message, delivered once; S's reply follows.

**Why it cannot be repaired away at the owner.**
- **Hoist the publication before the block.** This is forbidden: a sender seeing a published
  waiter must see a `Blocked` TCB (U9-RX3, "THE ORDERING IS FORCED BY SOURCE").
- **Hold rank 3 across Phases A and B.** This inverts lock order (3 before 1 and 2).
- **Take ranks 1→2→3 nested for take, block and publish.** This re-creates the serialized
  section the split removed.
- **A pre-park would-block read.** This was removed by U9-RECV-BLOCK2b because it created a worse
  window (`WouldBlock` for a finite wait).

The window from (1) to (5) is therefore structural, and (5)–(7) is the designed recovery. An
exclusion for VFS, a renamed marker, CPU pinning or a delay would hide it, not resolve it.

**What would still count as a defect.**
- Any race on the broad route (`endpoint=<number>`, or `d2_publish_race_unwinds` > 0).
- A split race without a message becoming visible on the receiver's endpoint (a spurious
  `QueueNonEmpty`).
- An unpaired or doubled race pair.
- An unwind that is not exact, or not restored (`queue_non_empty_unwound`, `resumable=0`).
- Any `IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED`.
- The raced message lost, or delivered twice.
- A sender never answered.
- A receiver that never runs again.
- A second live receiver on the endpoint (IPC-DISPLACE1).

**Proposed replacement rule** (falsifiable):
- **(B) Broad route.** The serialized broad receive arm never reports the race: no
  `D2_PUBLISH_RACE_UNWIND endpoint=<number>`, and `d2_publish_race_unwinds` stays 0.
- **(S) Split route.** Every `D2_PUBLISH_RACE_UNWIND endpoint=split tid=R` is accepted only if
  all of these hold, and is stop-ship otherwise:
  - **S1** R blocked first.
  - **S2** a message was queued on R's endpoint between R's receive entry and the race.
  - **S3** exactly one `D2_RECV_WAITER_RACE_UNWIND tid=R` pairs with it.
  - **S4** the unwind is exact and settles `queue_non_empty` / `restored` before R's next receive.
  - **S5** that record is bound exactly once in the boot, to R, after the race.
  - **S6** its reply terminal commits.
  - **S7** no fail-closed receive names R.

A prototype checker, `check_publish_race.py` (evidence branch, not wired into anything), accepts
the six events and rejects 33 controls derived from them, 11 per log: the broad spelling;
unpaired and doubled pairs; a non-exact unwind; an `unwound` settle; the enqueue removed; the
delivery removed or duplicated; the reply removed; a fail-closed line; and the block removed.

If accepted, the change is:
- §14.3 and its copies in `KERNEL_TEST_RULES` and this file's §3 and §8;
- the four smoke scripts' unconditional rejects, which become (B) plus a call to the checker for
  (S);
- the source guards that pin those rejects (`stage114_smoke_scripts_still_reject_…`,
  `stage115_smoke_scripts_still_check_…`, and the RISC-V capacity-rejection fixture's
  publish-race case). `stage115_d2_race_unwind_marker_is_zero` and
  `stage116_d2_publish_race_unwind_still_zero` only require that the broad marker exists, and
  stay true;
- the fd grader's RACE rule.

Then the next pass reruns the fd witness at SMP2 on three ports under the new rule, with the
negative controls above.

**Checked here.**
- On `4b7b71e0`, the 34 hosted tests that cover this transition pass. They include
  `u9rx3_forced_publish_race_unwinds_the_block_completely`,
  `the_queue_non_empty_branch_runs_the_exact_unwind_before_it_answers`,
  `the_publish_race_pair_is_logged_for_the_queue_race_only` and the `ipc_displace1` suite.
- The prototype's self-test passes.

**Not run**, because no code changed: the qualification matrix (live fd witness, core smokes,
1B witness, feature sweep).
