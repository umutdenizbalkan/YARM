#### IRQ-GENERATION1 → NOTIF-LIFETIME1 (from delivered `ff7cab55`)

**IRQ-GENERATION1 (hard stop, `wip/irq-generation1` `3b66b36`).** Revocation destroyed a
Notification by slot index alone. The broad and split revoke paths dropped the capability's
generation, so revoking a stale cap destroyed the newer incarnation in its slot: its routes, its
waiter and its generation. Revoking one cap of a SIGNAL/RECEIVE pair was enough to create the
stale cap. The reproducers G1–G6 failed 10 of 12 on `ff7cab55`. The audit then found the
documented single-owner contract was not enforced. The UART IRQ witness mints a third cap into
init, and delegation, fork inheritance and IPC transfer all copy Notification caps. Under
destroy-on-any-revoke, a second holder's revoke or exit destroyed everyone's object (L1–L3). The
package stopped for an owner decision.

**NOTIF-LIFETIME1 (owner-authorized: capability-reference lifetime).** The contract, the
accounting, the owners and the linearization points are in `doc/IPC.md` §8.8.

* *Model.* A per-slot reference count sits with the object at IPC rank 3. It is reserved before
  any capability is inserted at rank 4 and released after any capability is removed. The release
  is the one generation-checked destruction owner, shared by the broad and split paths.
* *Ranks.* Rank 3 and rank 4 are never held together. No new lock, syscall, capability type or
  side table was added, and no other object class gained a count.
* *Capability-space primitive.* `CapabilitySpace::revoke_reporting` reports every capability a
  revoke removes.
* *Removed code.* The index-only `destroy_notification` and `destroy_notification_split` are
  deleted.

Code `b481b3f`; regressions `6a04065`, `675a32c`, `fb27e94`.

**Audit, every Notification capability path.**

* *Creation:* `create_notification`, with a creation hold.
* *Insertion:* broad mint, split mint seam (also transfer materialization), broad delegation
  (also fork and spawn), split spawn mint and delegation.
* *Removal:* broad revoke, leaf delete, direct revoke; split revoke commits, bounded and heap;
  split mint rollback; provisional-cap release, broad and split; `release_delegation`, broad and
  split; broad cnode cleanup; no-alloc and split reap, which drop a cspace whole.
* *Fresh-cap rollbacks:* the delegation body's fresh-cap rollback is paired with its caller's
  release. The service-endpoint rollback names endpoints only.
* *Not Notification sites:* the reply fast-revoke sites are Reply-only; the `vm.rs` leaf delete
  is MemoryObject-only; `resize_slots` refuses to drop occupied slots.
* *Process-cnode grant release:* it only removes a space nothing was minted into.
* *Guards:* a site census and a closure guard pin all of this (73 capability closures; none
  reserves, releases or reaches IPC).

**Qualification of `675a32c`** (predeclared schedule; sequential; no step retried; logs, rcs,
artifact bindings and hashes kept):

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 427 warnings vs 428 on `ff7cab55` (the deleted destroy body carried one), no new warning |
| hosted lib (serial); integration (census + doc guards) | 6324 passed, 0 failed, 3 ignored; 41 binaries, 396 passed |
| subsets: notif_lifetime1, irq_generation1, stage22, stage23, u9f_split, ipc_displace1, fork, deleg, transfer, revoke, reap, spawn | 15, 15, 10, 6, 33, 19, 93, 46, 203, 77, 46, 138 passed |
| lock order (hosted sequential tracker) | on the tests both trees share, 476058 vs 475884 warnings (+174). The rise is `current=ipc previous=4` +238: the rank-3 release that now follows a rank-4 removal, sequential by design. The closure guard proves none is nested |
| default builds ×3 (`kernel_features=none`); recv-v2 oracles ×3 | pass |
| CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| UART/PL011 IRQ witness x86_64 / AArch64 / RISC-V | 3/3 `result=ok`, 8/8 deliveries each; `IRQ1_WITNESS_PROVISION_OK` (creation, cross-process mint into init, bind); no `NOTIF_REF_*` warning in any serial log |
| IPC-DISPLACE1 live witness SMP1 ×3 | 3/3 `witness=ok core_smoke=ok` |
| reply-direct matrix seal; server-dies ×3 (live process teardown) | pass |
| CI profiles on clean default bindings, F (kernel `9bcb98ea`) and B (`fcad6f90`) | smp2-core, smp4-core pass on both; sender-wake ×2 and sched-timeout fail with the inherited signatures on both. In F's smp2-sender-wake boot, SMP serial interleaving also split `X86_AP_KERNEL_CR3_BEGIN` (`X86_AP_TKlERNEL_CR3_BEGIN cpun=1`), although the AP came fully online; that result is kept. An appended run on the same kernel shows only the inherited `D6_SMP_DISPATCH_OK` |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1, #5, #6, identical on F and B |

Hosted negative controls, each removing one protection, all detected:

| Control | Protection removed | Detected by |
|---|---|---|
| NC1 | release ignores the generation | G1–G3, G6 and the owner test |
| NC2 | any release destroys | 51 tests |
| NC3 | reservation ignores the generation | stale publication onto a reused slot |
| NC4 | broad mint does not reserve | 52 tests |
| NC5 | failed mint keeps its reservation | failed-mint and creation tests |
| NC6 | reap drops caps unreported | both reap owners' test and the census |
| NC7 | cspace revoke stops reporting | 42 tests |
| NC8 | delegation does not reserve | 9 tests, including fork and transfer |
| NC9 | split mint does not reserve | 4 tests |
| NC10 | split rollback releases a slot it did not clear | split-seam test |
| NC11 | broad mint reserves after publishing | census order check, failed-mint tests |
| NC12 | a release moved inside a capability closure | closure guard |

**Limits.**

* No default image creates a Notification. The only production creator is the default-off UART
  IRQ witness.
* No live path revokes or reaps a Notification holder. Release-side evidence is hosted;
  server-dies exercises the converted teardown owners live without a Notification.
* The live reserve-versus-final-removal interleaving is shown with the production phases in
  order. It is not driven concurrently on two CPUs.
* Out of scope and unchanged:
  * the notification waiter can still be overwritten by a second receiver (NOTIF-WAITER1);
  * IRQ binding authority and exclusivity (IRQ-AUTH1).
* The six baseline debts remain as recorded.

Verdict: **NOTIF_LIFETIME1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p`
and `wip/notif-lifetime1` at the commit that adds this record; `wip/irq-generation1` `3b66b36`,
`main` `b81e0317` and every other ref unchanged.

