<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198E3 — Shared-Region IpcSend Live Cross-Architecture Seal

Wires the accepted post-lock shared-region transaction (`shared_region_execute`) into the real
direct and queued receive boundaries as two live retirement classes — `IpcSendSharedRegionDirect`
and `IpcSendSharedRegionEnqueue` — gated behind the oracle-proof knob so a normal boot is unchanged.

**This commit is the VERIFIED FOUNDATION slice** (the user chose to stage 198E3 like 198A). It
delivers the kernel-side production wiring + origin-gated markers + fail-closed fuse diagnostic +
hosted proof + the executable six-cell seal contract. The userspace live oracle and the 3-arch QEMU
six-cell run are the remaining live-wiring continuation (see "Remaining live work").

## Post-lock channel + origin-neutral executor reuse

A new `DispatchPostWork::BlockedWaiterSharedRegionDelivery` variant carries the accepted owned
`RecvBoundarySharedRegionSnapshot` (frozen object identity, attenuated rights, descriptor, receiver
generation authority, mapping intent, transferred pin, recv-v2 meta source, and the `origin_direct`
class marker) plus the endpoint index. The drain executor `execute_blocked_waiter_shared_region_delivery`
runs the SAME `shared_region_execute` for both origins — no fork, no duplicate executor or rollback.

## Direct production wiring (blocked receiver)

`produce_blocked_waiter_shared_region_delivery` (in the IpcSend boundary `Ok(false)` arm): for an
`OPCODE_SHARED_MEM` send to an already recv-v2-blocked receiver, under the knob + a trap drainer, it
consumes the waiter's blocked state, resolves the bound endpoint, and builds the snapshot via
`shared_region_phase_a(origin_direct=true)` — consuming the transfer envelope ONCE and moving its
object pin into the snapshot with no reference gap. It publishes exactly ONE post-work item. NO map,
TLB op, or user copy under the broad borrow. Off the knob → `Ok(false)` → the legacy path runs.

## Queued production wiring (dequeue)

`produce_queued_shared_region_delivery`: at the receiver-side dequeue the queued message is popped
once, the matching generation-checked envelope consumed once, its pin moved into a snapshot with
`origin_direct=false`, and the SAME post-work type published for the SAME executor.

## Origin-gated markers

A per-CPU `SharedRegionLiveOrigin` (direct/enqueue), set by the producer and consumed once by the
drain, gates all shared-region live markers. Ordinary-cap, reply-cap, plain, hosted-test, and
legacy-fallback paths never set it, so they can never emit these markers. On a real successful
post-lock completion the executor emits (arch-tagged, ≤192-byte DebugLog):

```
IPCSEND_SHARED_REGION_OBJECT_OK    arch=<a> class=<c> object_match=1 fresh_cap=1 pin_transfer=1
IPCSEND_SHARED_REGION_MAP_OK       arch=<a> class=<c> map_right=1 write_right_ok=1 nx=1 cleanup_token=1
IPCSEND_SHARED_REGION_LIFECYCLE_OK arch=<a> class=<c> transaction_published=1 receiver_wakes=1 leaked_state=0
GLOBAL_LOCK_RETIRE_CLASS_BEGIN/DONE arch=<a> class=IpcSendSharedRegion<Direct|Enqueue> result=ok
```

## Cancellation fuse

The accepted fail-closed overflow fuse is preserved (permanent per-instance). A one-shot diagnostic
`SHARED_REGION_CANCEL_FUSE_SET reason=capacity_exhausted result=fail_closed` is emitted exactly once
on the clear → set transition (in `shared_region_request_cancel`), never auto-cleared. A normal live
oracle run must show `count=0`; hosted tests cover the saturation behavior (never in QEMU).

## RISC-V typed outcomes

Shared-region senders stay on the canonical in-lock publication + post-lock drain; the sender
continues (`ReturnToCurrent`) and a receiver that blocks with nothing runnable idles via
`EnterKernelIdle { reason: BlockedIpcNoRunnable, class: IpcRecv }` — never `Err(Internal)` as idle
control flow (guarded by a hosted source-contract test).

## Hosted proof

`stage198e3_shared_region_live` (7 tests): direct live-work publication origin; enqueue live-work
publication origin; marker origin gating; one post-work item per transaction; RISC-V
ReturnToCurrent / inert-without-knob contract; seal-parser contract; fuse diagnostic emitted once.
`scripts/qemu-shared-region-live-seal.sh` is the executable six-cell seal contract:
`SECOND_COHORT_SHARED_REGION_SEAL arches=3 classes=2 live_cells=6 fuse_trips=0 result=ok`.

## Preserved

`SYSCALL_COUNT=32`, `VARIANT_COUNT=22`, NR27 absent, DebugLog=192,
`REPLY_CAP_QUEUEING_SUPPORTED=false`; no new syscall / ABI / lock / capacity / capability-transfer
variant. All existing QEMU seals stay green (the producers are INERT off the knob, which no existing
boot sets). No D2 / IpcCall / Reply / timeout / notification / D3 / D6 work.

## Remaining live work (198E3 continuation, before the seal is green)

1. **Decompose `shared_region_execute` into `&SharedKernel` seams** (mint / `map_user_page_split` /
   `copy_to_user_split` / wake) so the executor runs strictly outside every lock — the drain arm
   currently re-enters `with_cpu`, which is fine while DORMANT but must move off-lock before the
   oracle enables a producer (hard-stop: user copy under locks).
2. **Userspace shared-region live oracle** (`init/service.rs`, gated): direct (receiver blocks
   first; sender transfers a MemoryObject/DmaRegion; receiver observes mapped bytes + fresh cap;
   cleanup token releases the mapping once) and enqueue (sender sends before receiver waits; later
   dequeue + map; duplicate release rejected). At least one class uses a multi-page region.
3. **Per-arch boot provisioning** (slot-5 selector) + wire `produce_queued_shared_region_delivery`
   into the recv-v2 / RecvSharedV3 dequeue site.
4. **Run**: build fresh fail-closed artifacts (x86_64/AArch64/RISC-V) once, one combined
   direct+enqueue oracle per arch, then the six-cell seal once.
5. Keep the direct/race/enqueue hosted seals and the first-cohort seal green; read-only mapping is
   sufficient for the primary live seal (hosted tests already cover the writable-right gate).

## Hard-stops honored (this slice)

No user copy under locks or mapping-before-provisional-registration is REACHED (the producers are
gated off the knob, so the executor arm is dormant in every boot and hosted test); no duplicate work
publication (exactly one stash per transaction); no mapping after cancellation (the accepted
checkpoints are unchanged); no fuse trip in any non-saturating path; RISC-V outcome contract
preserved; no reply-cap queueing activation (`REPLY_CAP_QUEUEING_SUPPORTED=false`).
