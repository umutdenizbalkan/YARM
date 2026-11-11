#### FUTEX-FOUND1 (from delivered `a7a27fb4`)

**Root cause.** Futex identity was a numeric virtual address. A waiter was a TCB in
`Blocked(Futex(VirtAddr))`, and a wake scanned the TCB array for that address. So:

* a private wake in one address space woke another process's waiter at the same VA (F1);
* one MemoryObject mapped by two processes at different VAs had no common identity (F3), while two
  unrelated objects at the same VA aliased (F4);
* a replacement address space reusing a destroyed ASID number would see the old waiters (F10);
* wakes ran in TCB-array order, not arrival order (F11);
* robust-futex cleanup woke in the namespace of whichever task drove the exit (F14);
* the split wake left a waiter `Runnable` and unqueued when its enqueue plan failed, and the broad
  wake's `?` could abandon later waiters after one refused enqueue.

Two existing tests (`futex_wait_blocks_current_and_wake_requeues_waiter` and the
externally-driven robust test) asserted the cross-process wake, and the overtaken witness relied
on it (two processes sharing a raw frame).

**Audit (§2, Q1–Q12), in brief.** Wait read the word under the current ASID through the BL3b
two-read protocol, which is sound because the registration and the wake scan already shared the
task lock. Wake had no key, no FIFO and no shared support. Robust cleanup's registration has no
production caller (`set_robust_futex_head` is test-only), so that defect is reachable only from
tests today. VM identity is available without a HARD-STOP: `frame_owner_slot_locked` names the
allocator-owned object containing a frame (extents are disjoint), MemoryObject ids only advance,
and an address-space generation is a bounded addition to `AddressSpaceManager`. Initramfs slices
are borrowed, not allocator-owned, and are refused as shared keys (`WrongObject`).

**Keys** (`boot/futex.rs`). `FutexKey::Private { asid, aspace_generation, va }` and
`FutexKey::Shared { object_id, offset }`. `resolve_futex_key_locked` /
`resolve_futex_word_locked` resolve the key — and read the word — in ONE vm (rank 5) then memory
(rank 6) acquisition, from one mapping. No identity is a physical address alone. A shared wait on
memory no allocator-owned object backs is refused, never made private.

**Waiters.** `FutexWaiterTable`, task domain (rank 2, `task_state_lock`; classified in BLR1's
field authority): `MAX_FUTEX_WAITERS = MAX_TASKS` record slots with per-slot generations, 64 hash
buckets of singly linked FIFO lists, no allocation. A record carries its key, tid, TCB slot and VA.
The TCB carries `Blocked(Futex(FutexWaitToken { slot, generation }))`. **The TCB is the
authority:** a record is live only while its TCB holds the same token, so a stale record is inert
and is reclaimed by the claim that meets it, by the registration's sweep, or eagerly at exit and
death. Every liveness check is O(1) (the record names its TCB slot; the tid and the token make it
exact). A future requeue moves a record between buckets without touching the TCB.

*Capacity.* Each task waits at most once, so a live record per possible task cannot overflow. The
`Full` refusal is still implemented and is decided before anything is written (F12).

*Owners.* Exactly three functions write a futex waiter's status, and both routes reach them:
`register_futex_waiter_locked` (`Running -> Blocked(Futex(token))`, exact `{tid, asid}`, record
and status in one critical section), `undo_futex_registration_locked` (back to `Running`, only
from exactly that token), and `claim_futex_waiters_locked` (oldest first, up to `count`, each
`Runnable` and its record retired in one critical section). The five writes they replace — two
broad and three split — are gone; the WA2A/WA2B censuses go 41 → 39 and 47 → 45, with the class
partition CANNOT 19 → 18 and INTO_BLOCKED 10 → 9.

**Compare-and-park.** The split transaction: (0) resolve the key and word; a mismatch is
`ValueChanged` with nothing written. (1) Register (rank 2). (1b) Re-resolve the key and word; if
either changed, undo and answer `ValueChanged` / `WordUnreadable`. (2) Compare-and-clear the
current slot (rank 1); a refusal undoes the registration (`VictimChanged`). (3) Read back by token
(rank 2): a waker that claimed in the window is detected, and the caller is restored through the
existing placement recovery (`WokenDuringPublication`). *Ordering proof:* the registration and a
waker's claim are serialized by the task lock, and a waker stores the word before it claims.
Either the claim follows the registration and finds the waiter, or it precedes it — and then the
store precedes the waker's release of the task lock, which precedes the registration's
acquisition, which precedes the re-read, so the re-read sees the new value. No two of these
acquisitions nest. The broad owner holds the broad lock across its read and its park.

**Lifecycle.** Exit and death retire the task's records eagerly after their status writes. Robust
cleanup (`futex_wake_on_exit(exiting_tid, addr)`) resolves each robust word in the EXITING task's
address space, under its private key and, when an object backs it, its shared key. A waiter pins
nothing; when its backing is reclaimed it stays parked on a key nothing live can resolve to, until
its task ends, when its record is retired exactly once (F10b). This is recorded as a design
decision in `KERNEL_UNLOCK_DEFERRED_DEFECTS.md`.

**ABI** (no compatibility wrappers). `FutexWait(addr, expected, flags)`,
`FutexWake(addr, count, flags)`; flags bit 0 `FUTEX_FLAG_SHARED`, every other bit reserved
(`InvalidArgs`); `addr` 4-byte aligned; one decode (`decode_futex_args`) for both routes. The
caller-supplied `observed` is removed. Slots 3+ are not read: a deadline, a requeue target or a
wake mask will each be selected by a reserved flag bit. Callers migrated: `yarm-user-rt`
(`FutexScope`), `init_server` (all oracle and hand-off words are thread-local: private), the
compat hooks, the AP-scheduling witness programs (x86_64, AArch64, RISC-V: flags 0), and the
overtaken witness programs (x86_64, AArch64), whose mailbox is now an anonymous MemoryObject
mapped in both processes and waited on with flags 1.

**Re-derived tests.** Each kept its reason in place. Behavioural: the cross-AS wake test now
asserts that the private wake from another space finds nobody and the owner-namespace wake finds
the waiter; the robust test was re-derived in the victim's space; the wake-during-publication test
now forces the window with a test-only phase hook through the production owners. Source guards:
191b (one keyed claim), 191d (one range policy, one predicate, no `observed`, one registration
owner, the token used by undo and read-back), 195c/196e/BL3a (scoped oracle calls), 197, u9qa (six
non-parking outcomes, one more for `CapacityExhausted`), and the 199D censuses above.

**Reproducers** (`futex_found1`, 16 tests). On `a7a27fb4` (baseline module
`futex_found1_baseline`, kept in the evidence): **failed** F1, F3, F4, F10, F11, F14; **passed** F2,
F5/F6, F8 (head removal), F13; **not exercisable** F7 (no window hook), F9 (no per-cycle identity),
F12 (no waiter store). F15 is the live boot matrix; its hosted half checks that the encoded ABI is
the decoded ABI. All 16 pass on the candidate. F8 was strengthened (`5db787b8`) to remove the
middle waiter, which a head removal cannot distinguish from a broken unlink.

**Hosted negative controls.** Each control reintroduces one defect class in a disposable worktree
(`ff1_nc.py`, at `5db787b8`; the qualified candidate differs from it only by the plan-refusal
marker's name). All ten were detected, each by behavioural tests; the unmodified tree passes all
132 tests in the subset (`futex`, `robust`).

| Control | Defect reintroduced | Detected by |
|---|---|---|
| NC1 | numeric-VA-only matching (the private key forgets its address space) | F1, F2, F9, F10, F14, the re-derived cross-AS and robust tests |
| NC2 | the address-space generation dropped from the private key | F2, F9, F10, the robust test |
| NC3 | the shared key's offset dropped | F3 |
| NC4 | no re-read once the waiter is registered (non-atomic compare/publication) | F6 |
| NC5 | duplicate claim: the claim neither checks the waiter nor retires its record | F2, F8, F9, F11, F13, 191b split semantics, 191d publish/woken-during-publication |
| NC6 | the undo and the read-back accept any futex wait (stale wait generation) | F9 |
| NC7 | unlink forgets the predecessor's link (incorrect FIFO removal) | F8 |
| NC8 | capacity refusal after the status write (no rollback) | F12 |
| NC9 | claimed and counted but never made `Runnable` | 17 tests: F1–F3, F5, F7–F9, F11, F13, F14, and the 191b/191d/192a wake tests |
| NC10 | robust cleanup in the DRIVING task's address space | F14, the re-derived cross-AS test, two 191d continuation tests |

**Qualification of `e684de30`.** A predeclared schedule (`ff1_schedule.md`, hashed with the
runner and scripts); steps ran sequentially; nothing was retried in place. One rerun was
appended (R1, below). Baseline B is `a7a27fb4`.

| Step | Result |
|---|---|
| fmt; clippy `--features hosted-dev` | clean; 424 warnings against 427 on B, identical class set (three fewer unused imports) |
| hosted lib (serial) | 6428 passed, 0 failed, 3 ignored |
| integration (`--no-fail-fast`): broad-lock census (with `futex_waiters` classified), doc-fragmentation guard | 41 binaries, 396 passed |
| subsets: futex_found1, futex, robust, stage191b, stage191d, u9qa, stage199d, overtaken, kthread, fork, vm_, scheduler, sched_, exit, reap, spawn, thread, generation, shared_region, ipc_displace1, sender_wake | 16, 131, 5, 5, 20, 47, 427, 22, 42, 93, 167, 103, 37, 320, 47, 139, 74, 88, 178, 19, 91 passed, 0 failed (the `apsched` and `task_lifecycle` filters match no hosted test; the live witnesses below cover them) |
| lock order, common test set | 23 changed (test, pair) entries: 21 in futex tests (class a), 2 in `c09_reply_vs_timeout_terminal_cas_race` (class b), none elsewhere; nothing nested |
| default builds ×3; recv-v2 oracles ×3; CI-exact strict builds + `QEMU_SMOKE_STRICT=1` core smokes ×3 | pass |
| IPC-DISPLACE1 live witness SMP1 ×3 (sender-waiter) | `witness=ok core_smoke=ok` |
| first-cohort retirement seal on CI-exact artifacts (R1): FutexWake, FutexWait switch, FutexWait idle and Yield oracles on three ISAs | **F: 12/12 live cells, `FIRST_COHORT_CROSS_ARCH_SEAL arches=3 result=ok`**; B: 12/12 live cells, but `riscv64 FutexWait idle oracle proof missing` (its RISC-V idle boot emitted no `RISCV_FUTEX_WAIT_IDLE_ORACLE_DONE`), the run-to-run variance `STATUS.md` already records with bit-identical RISC-V binaries; F passed it |
| overtaken witness x86_64 and AArch64 — W on CPU 0 in one process, K on CPU 1 in another, one MemoryObject, a shared key | `OVT_WITNESS_SEAL result=ok` on both, 4/4 rounds overtaken, CPU placement graded; same on B |
| AP-scheduling witness SMP=2 ×3; KTHREAD1 witness SMP=2 ×3 | `APSW_WITNESS_SEAL result=ok` ×3; `KT1W_SEAL result=pass` ×3 |
| UART (x86_64), PL011 (AArch64), UART (RISC-V) IRQ witnesses | 8 of 8 deliveries each |
| reply-direct matrix seal; server-dies ×3 | pass |
| CI profiles on F and B: sender-wake, d2-send, smp2-core, smp4-core, smp2/smp4-sender-wake, sched-timeout | identical status and identical error sets on F and B: sender-wake and smp2/smp4-core pass; inherited #2, #3, #4 and the d2-send observation (8 errors) |
| reply-timeout matrix; SMP-request script; CI strict timer grep | inherited #1 (`class=IpcReplyTimeout`), #5 (line 67) and #6 (0/0/0), identical on F and B |
| every QEMU log of F for the marker list (now also `FUTEX_WAKE_ENQUEUE_DEFERRED`, `FUTEX_WAIT_PARK_REFUSED`, `reason=capacity_exhausted`, overtaken `result=fail`) | no hits |

*R1, the appended rerun.* The scheduled seal steps (FX1 on F, FB1 on B) ran on whatever artifacts
the preceding steps had left. The seal's strict x86 smokes require the CI-exact
`timer-contract-witness` initramfs and refused to boot (`ARTIFACT-BINDING ... result=refused`,
reproduced with output captured); on B the AArch64 and RISC-V smokes also wrote no logs. FX1
therefore reported 11/12 and FB1 3/12 — artefacts of the harness, not of either kernel. R1 rebuilt
CI-exact strict artifacts on each side and reran the seal; FX1 and FB1 are kept unaltered.

**Limits.**

* *Robust cleanup is hosted-proven only:* no production path registers a robust list (recorded).
* *No cancellation on teardown:* a waiter whose backing is reclaimed stays parked until its task
  ends (recorded as a design decision).
* *Not implemented, by scope:* timed waits, requeue / compare-and-requeue, bitset wake masks,
  priority inheritance or protection, donation, RT-SCHED1. Their ABI room is the reserved flag
  bits and slots 3+.
* *Historical records* (`KERNEL_LOCKING.md`, older `KERNEL_UNLOCKING.md` stages) still describe
  the `observed` argument as it was; the current-state documents are updated.
* *Still open:* the six baseline debts and the inherited `d2-send` failure, unchanged; the
  ENDPOINT-ROLLBACK1 buffered-message observation, unchanged.

Verdict: **FUTEX_FOUND1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/futex-found1` at the
commit that adds this record; `main` `b81e0317` and every other ref unchanged.
