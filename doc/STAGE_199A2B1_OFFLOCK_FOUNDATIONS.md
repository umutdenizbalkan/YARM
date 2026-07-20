# Stage 199A2B1 — x86_64 Off-Lock IpcCall/IpcReply: Foundations Landed + Live-Oracle Blocker

Goal of the stage: move the direct NR6 request and NR7 reply paths off the broad
runtime lock on x86_64 and prove both in one genuine QEMU round-trip oracle.

This document records, honestly, **what landed and is tested in this increment**
and **the exact remaining work** required before a genuine clean-QEMU seal can be
reported. No QEMU seal is claimed here — per the task's own instruction ("Report
the live seal only after a genuine clean QEMU log. Otherwise report the exact
blocker and last valid marker").

---

## Landed and tested in this increment

### Part 1 — Teardown identity correction (COMPLETE)

Reply-record cleanup now flows through **authoritative identity-typed entry
points** that match the supplied `ReceiverWaiterIdentity { tid, asid }` verbatim
and do **not** re-resolve an ASID from a numeric TID inside the cleanup body:

* `revoke_reply_caps_for_caller_identity(caller: ReceiverWaiterIdentity)` — matches
  `record.caller == {tid, asid}`.
* `revoke_reply_caps_for_replier_identity(replier: ReceiverWaiterIdentity)` — matches
  `record.responder_tid == Some(replier.tid)` and the stored `replier_asid` (a
  record with no stored replier ASID carries no incarnation evidence and matches on
  the numeric TID — the never-leak direction).

Every production teardown site (`exit_task`, `restart_task`, `mark_task_dead`,
`reap_faulted_task_noalloc_cleanup` in `src/kernel/boot/restart_state.rs`) now
captures the exiting task's `{tid, asid}` **while its TCB is still live** (the
authoritative moment) and calls the identity entry points. A replacement task
reusing the numeric TID (different ASID) is therefore untouched. The numeric
wrappers remain as thin test/compat shims that resolve once and delegate — they
are never the production cleanup authority.

### Part 2 — Architecture-neutral owned snapshots + reversible reservation FSM (COMPLETE)

`src/kernel/ipccall_direct.rs` adds bounded, owned, `Copy`, by-value building
blocks (no `&mut KernelState`, no borrows, no raw userspace pointer):

* `IpcCallDirectSnapshot { caller, send_endpoint_cap, reply_endpoint_cap,
  payload: [u8; 128], payload_len }` and
  `IpcReplyDirectSnapshot { replier, reply_cap, payload: [u8; 128], payload_len }`.
  Their `build(..)` constructors copy the source bytes into the owned buffer and
  return `None` on over-length — a length rejection produces **no snapshot** and
  therefore mutates no IPC / capability / waiter / reply-record / scheduler state.
* `ReplyReservation` — the **reversible one-shot** state machine
  `Available → Reserved { reservation_generation, replier } → Consumed` with:
  * `reserve` (fails `NotAvailable` on a duplicate/aliased invocation while
    Reserved/Consumed — no copy, no wake),
  * `release` (`Reserved → Available` caller-copy-fault rollback; reply authority
    stays usable; zero wake),
  * `consume` (`Reserved → Consumed`, requires the matching reservation generation
    **and** the bound replier identity; one-shot — a second consume fails).

These are unit-tested in the module (8 cases) and guarded by hosted tests
(`stage199a2b1_offlock_foundations`, 7 cases).

### Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192,
`REPLY_CAP_QUEUEING_SUPPORTED=false`, Stage 198F 10 classes / 30 cells. No live
NR6/NR7 split-dispatch arm was added, so the existing broad-lock fallback is
byte-for-byte unchanged and the direct classes are trivially default-off.

---

## NOT landed — the exact remaining blocker for the QEMU seal

The genuine off-lock live round-trip oracle is deep, multi-component work that
this increment does not complete. Remaining, in dependency order:

1. **x86 trap-entry publish (Part 2 wiring).** Wire the pre-global-lock x86 seam
   (`try_split_dispatch_into_frame`) to, for the oracle-gated direct classes,
   snapshot NR6/NR7 args, validate length, copy the source userspace payload with
   `copy_from_user_asid_split_read` (no lock held), and publish the owned
   `IpcCallDirectSnapshot` / `IpcReplyDirectSnapshot`. This must reuse the accepted
   `DispatchPostWork` per-CPU stash + post-lock executor rather than a new
   mechanism.

2. **NR6 direct request transaction (Part 3).** Off-lock: revalidate caller
   identity + caps → require an EXACT committed blocked server waiter `{tid,asid}`
   (else canonical `WouldBlock` **before** reserving) → reserve reply record → bind
   caller+replier identities → prepare one receiver-local reply cap → copy request
   to server outside all locks → finalize server blocked state + exact waiter →
   enqueue server last → return sender to its own frame. Full rollback of record /
   reply-cap slots / transfer envelope / server mutation / post-work on any
   post-reservation failure, leaving the server consistently blocked.

3. **NR7 reversible reply transaction (Part 4)** backed by `ReplyReservation`:
   copy reply outside locks → resolve reply object index+generation → validate
   bound replier `{tid,asid}` → require exact caller reply-endpoint waiter
   `{tid,asid}` → reserve → copy reply to caller outside locks → final revalidation
   → consume → revoke aliases → clear caller blocked state + waiter → Runnable →
   enqueue caller last. On caller-copy fault: `Reserved → Available`, caller stays
   blocked, zero wake. On caller-exit / endpoint-generation change while reserved:
   cancel/consume, reclaim authority, zero wake, single cleanup owner.

4. **Split-dispatch constraints (Part 5).** The x86 direct paths must contain no
   `with`/`with_cpu`/broad `&mut KernelState`, no user copy under any lock, no
   scheduler enqueue under IPC/capability/task locks, and no fallible op after
   enqueue — reusing `ReceiverWaiterIdentity`, the exact endpoint-waiter claim, the
   claim-before-register-clear finalization, the ranked split seams, and the
   post-lock drain.

5. **Live oracle gates + provisioning (Parts 6–7).** Default-off feature
   `x86-ipccall-direct-oracle` + selector `yarm.x86_64_ipccall_direct_oracle=1`
   (require x86_64 + BSP + `QEMU_SMP=1` + active split dispatcher); boot
   provisioning of the two endpoints/caps; a two-task userspace round-trip oracle
   (server blocks on request endpoint → client NR6 → client blocks on reply
   endpoint → server resumes with request + fresh reply cap → server NR7 → client
   resumes once) using authoritative committed-waiter acknowledgements (no
   timing-based blocked proof).

6. **Markers + seal + smoke (Part 8) and QEMU proof.** The success-only kernel
   markers, the `X86_IPCCALL_DIRECT_ROUNDTRIP_DONE …` userspace completion, the
   `scripts/qemu-ipccall-reply-direct-x86_64-smoke.sh` runner, and finally a fresh
   clean QEMU boot emitting
   `STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=x86_64 classes=2 live_cells=2
   duplicate_wakes=0 result=ok`.

### Last valid marker

None from a live oracle — the live oracle is not yet wired, so there is no
`IPCCALL_DIRECT_REQUEST_OK` / `IPCREPLY_DIRECT_OK` marker to report. The genuine
seal is intentionally **not** emitted. The foundations above are the verifiable,
committed progress toward it.
