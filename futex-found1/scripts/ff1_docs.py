#!/usr/bin/env python3
"""FUTEX-FOUND1 documentation commit. Usage: ff1_docs.py <worktree> <record.md>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); record = pathlib.Path(sys.argv[2]).read_text()
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:90], s.count(old))
    p.write_text(s.replace(old, new))

# ── doc/SYSCALL_ABI.md ─────────────────────────────────────────────────────────────────────────
A = "doc/SYSCALL_ABI.md"
patch(A, "| `9` | `FutexWait` | public (`arg0=addr`, `arg1=expected`, `arg2=observed`) |",
         "| `9` | `FutexWait` | public (`arg0=addr`, 4-byte aligned; `arg1=expected`, `u32`; `arg2=flags`) — FUTEX-FOUND1 |")
patch(A, "| `10` | `FutexWake` | public (`arg0=addr`, `arg1=max_wake`) |",
         "| `10` | `FutexWake` | public (`arg0=addr`, 4-byte aligned; `arg1=count`, `u32`; `arg2=flags`) — FUTEX-FOUND1 |")
patch(A, '''- `FutexWait(addr, expected, observed)` returns `0` without blocking when `expected != observed`, and otherwise blocks only while the word at `addr` still holds `expected`: the kernel reads the word itself, and checks it again after the waiter is registered, so a `FutexWake` issued after a store to the word can never be lost. A word that no longer holds `expected` returns `0` (not blocked); `1` means the caller blocked and was woken.''',
'''- **ABI (FUTEX-FOUND1).** `FutexWait(addr, expected, flags)` and `FutexWake(addr, count, flags)`.
  `addr` must be 4-byte aligned and `expected` / `count` must fit in `u32`. `flags` bit 0 is
  `FUTEX_FLAG_SHARED`; every other bit is reserved and a set reserved bit is `InvalidArgs`. All
  three argument errors are `InvalidArgs` and are answered before the address range or the caller
  is consulted. Slots 3 and up are not read: a later operation (an absolute deadline, a
  compare-and-requeue target, a wake mask) is selected by a flag bit that is reserved today, and
  that bit is what gives meaning to the further slots. There is no caller-supplied `observed`.
- **Keys.** A *private* futex (`flags = 0`) is `{address space, its generation, va}`: threads of one
  address space share it, and nothing in another address space — including a later address space
  that reuses the same ASID number — can reach it. A *shared* futex (`flags = FUTEX_FLAG_SHARED`)
  is `{MemoryObject id, offset}`: every process that maps the object, at any VA, reaches the same
  waiters. Object ids are never reused. A shared wait or wake on memory that no allocator-owned
  MemoryObject backs is refused with `WrongObject`; it is never silently treated as private.
- `FutexWait` blocks only while the word at `addr` still holds `expected`: the kernel reads the
  word itself, in the same acquisition that resolves the key, and reads it again after the waiter
  is registered, so a `FutexWake` issued after a store to the word can never be lost. A word that
  no longer holds `expected` returns `0` (not blocked); `1` means the caller blocked and was woken.
  A return of `1` is a wake of the same key, so callers re-check their condition.
- `FutexWake` wakes up to `count` waiters of the key `addr` resolves to in the caller's address
  space, oldest first, and returns how many it woke. `count = 0` wakes nobody.
- A waiter table slot exists for every possible task, so a wait cannot run out of waiter storage.
  The capacity refusal is still implemented (`TaskTableFull`, decided before anything is written).
- Robust-futex cleanup at task exit wakes each robust word in the EXITING task's own address space,
  under its private key and, when an object backs the word, its shared key.''')

# ── doc/SCHEDULING_CONTRACT.md ─────────────────────────────────────────────────────────────────
C = "doc/SCHEDULING_CONTRACT.md"
patch(C, "| FutexWake | `futex_wake_split_mut` | trapping CPU | 2 (scan), then 1 per woken | masked | caller continues | ≤ 512 TCB scan; ≤ `max_wake` enqueues |",
         "| FutexWake | `futex_wake_split_mut` → `futex_wake_key_split` | trapping CPU | 5 → 6 (key), then 2 (keyed claim), then 1 per woken | masked | caller continues | ≤ 512 record visits in one bucket, O(1) each; ≤ `count` enqueues (FUTEX-FOUND1) |")
patch(C, "| W10 | FutexWake | `futex_wake_split_mut` | **S** | ≤ 512 TCB scan + ≤ `max_wake` (≤ 512) enqueues. |",
         "| W10 | FutexWake | `futex_wake_split_mut` | **S** | one key resolution, then ≤ 512 record visits in one hash bucket (each an O(1) TCB-slot check) + ≤ `count` (≤ 512 live waiters) enqueues. FUTEX-FOUND1. |")

# ── doc/KERNEL_MULTITHREADING_DESIGN.md ────────────────────────────────────────────────────────
M = "doc/KERNEL_MULTITHREADING_DESIGN.md"
patch(M, '''- `futex_wait_current(addr, expected, observed)`
- `futex_wake(addr, max_wake)`''', '''- `futex_wait_current(addr, expected, kind)` (`kind`: `FutexKind::Private` or `Shared`)
- `futex_wake(addr, count, kind)`''')
patch(M, '''1. validating the futex word/address contract,
2. marking the current TCB as `Blocked(WaitReason::Futex(addr))`,
3. removing it from the running slot,
4. dispatching the next runnable thread.

A futex wake scans blocked TCBs for matching futex addresses, marks them runnable, and re-enqueues them.

This is intentionally simple (`O(n)` wake scan over the fixed TCB table) but it is **real scheduler-integrated blocking**, unlike the previous compatibility-only bookkeeping shim.''',
'''1. resolving the word's futex KEY (private: address space, its generation and VA; shared:
   MemoryObject and offset) and reading the word, in one vm-then-memory acquisition,
2. registering a keyed waiter record and marking the current TCB `Blocked(WaitReason::Futex(token))`
   in one task-lock critical section — the token names the record slot and its incarnation,
3. re-reading the word (a changed word undoes the registration and returns without blocking),
4. removing it from the running slot, and dispatching the next runnable thread.

A futex wake resolves the same key in the waker's address space and claims up to `count` waiters
of that key, oldest first: each is made `Runnable` and its record retired in one task-lock
critical section, then each is enqueued once. Waiters live in a bounded, allocation-free table
(`boot/futex.rs`, FUTEX-FOUND1) with one slot per possible task; a record is live only while its
TCB holds the same token.''')
patch(M, '''- `futex_wait_hook` -> `futex_wait_current`
- `futex_wake_hook` -> `futex_wake`''', '''- `futex_wait_hook` -> `futex_wait_current` (private key)
- `futex_wake_hook` -> `futex_wake` (private key)''')

# ── doc/KERNEL_TEST_RULES.md ───────────────────────────────────────────────────────────────────
T = "doc/KERNEL_TEST_RULES.md"
patch(T, '''// ... then state.futex_wake(0x1000, 1) fails with TaskMissing''',
         '''// ... then state.futex_wake(0x1000, 1, FutexKind::Private) fails with UserMemoryFault''')
patch(T, '''### N+22.3 — Robust futex wake must not depend on current_tid ASID

Tests that call `exit_task` on a non-current task (supervisor-driven exit)
must not fail due to ASID mismatch in the futex wake path.  Use
`futex_wake_on_exit` (no ASID validation) for the robust list wake loop.''', '''### N+22.3 — Robust futex wake uses the EXITING task's namespace

Tests that call `exit_task` on a non-current task (supervisor-driven exit) must
see the robust words woken in the exiting task's own address space, never the
driver's. `futex_wake_on_exit(exiting_tid, addr)` resolves the private key (and
the shared key, when an object backs the word) in `exiting_tid`'s address space;
it needs the key only, so an unwritten hosted word is not an error there
(FUTEX-FOUND1).''')
patch(T, '''### N+22.5 — futex_waiter_count relies on TCB status, not a separate list

`futex_waiter_count` counts tasks in `Blocked(WaitReason::Futex(addr))` status.
Tests must not assume a separate futex waiter list is maintained.  A task that
transitions from `Blocked(Futex)` to any other status is no longer counted.''', '''### N+22.5 — futex waiters are keyed records; the TCB token is the authority

FUTEX-FOUND1: a futex waiter is a record in the keyed waiter table, and the TCB
holds `Blocked(WaitReason::Futex(token))`. A record is live only while its TCB
holds the same token, so a task that leaves `Blocked(Futex)` by any route is no
longer counted, whatever record it left behind. `futex_waiter_count(va)` counts
LIVE records that named `va`, under any key. To assert "parked at this VA" use
`futex_parked_at_va_for_test(tid, va)`; to publish a waiter without running the
syscall use `inject_futex_waiter_for_test(tid, va)`, which registers through the
production owner. Never write `Blocked(Futex(_))` into a TCB directly and expect a
wake to find it: no record names it. To force the publication window (a wake
between registration and the compare-and-clear) arm
`futex::futex_phase_hook::arm_after_register`.''')

# ── doc/AI_AGENT_RULES.md ──────────────────────────────────────────────────────────────────────
R = "doc/AI_AGENT_RULES.md"
patch(R, '''  reference to an object is its own class's `{index, generation}`: an IRQ route names a
  Notification and can never refer to an Endpoint.
''', '''  reference to an object is its own class's `{index, generation}`: an IRQ route names a
  Notification and can never refer to an Endpoint.
- A futex is its KEY, never its VA or its physical frame (FUTEX-FOUND1, `doc/SYSCALL_ABI.md`
  "Futex safety contract"). Private: `{asid, address-space generation, va}`; shared:
  `{MemoryObject id, offset}`. Resolve keys only through `boot::futex::resolve_futex_key_locked`
  / `resolve_futex_word_locked`, and read a word only in the acquisition that resolved it. Waiter
  status is written by exactly three owners — `register_futex_waiter_locked`,
  `undo_futex_registration_locked`, `claim_futex_waiters_locked` — and every check of a waiter
  compares the exact token. Never wake a futex waiter by TID or by VA.
''')

# ── doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md ──────────────────────────────────────────────────────
D = "doc/KERNEL_UNLOCK_DEFERRED_DEFECTS.md"
s = (wt / D).read_text().rstrip("\n") + "\n"
s += ("- 2026-11-11 | RESOLVED (FUTEX-FOUND1) — futex waiters were matched by numeric VA alone: a "
      "private wake in one address space woke another process's waiter at the same VA; a "
      "MemoryObject mapped by two processes at different VAs could not synchronise; a replacement "
      "address space reusing an ASID number would inherit waiters; wakes ran in TCB-array order; "
      "robust cleanup woke in the namespace of whichever task drove the exit. Resolution: "
      "private keys `{asid, address-space generation, va}`, shared keys `{object id, offset}`, a "
      "bounded keyed FIFO waiter table whose records are live only while their TCB holds the "
      "same token, and robust cleanup in the exiting task's keys. Confirmed by F1, F3, F4, F10, "
      "F11 and F14 failing on `a7a27fb4` | src/kernel/boot/futex.rs, exec_state.rs, runtime.rs, "
      "restart_state.rs | found and resolved in FUTEX-FOUND1\n")
s += ("- 2026-11-11 | DESIGN DECISION, NOT A DEFECT (FUTEX-FOUND1) — a futex waiter pins nothing: "
      "no frame, no refcount. When the memory a waiter's key names is unmapped or reclaimed, the "
      "waiter stays parked on a key nothing live resolves to (object ids and address-space "
      "generations are never reused, so no new mapping can reach it) until its task exits or is "
      "killed, when its record is retired exactly once. It is not cancelled: no generation-exact "
      "teardown wake is delivered, because a task whose address space is gone cannot be resumed "
      "into it. A future cancellation mechanism (with a defined return value) would be a new "
      "operation | src/kernel/boot/futex.rs | FUTEX-FOUND1\n")
s += ("- 2026-11-11 | OPEN OBSERVATION (pre-existing, unchanged) — no production path registers a "
      "robust-futex list (`set_robust_futex_head` has no production caller), so the robust-cleanup "
      "repair is reachable today only from tests; it is ready for the registration path when one "
      "exists | src/kernel/boot/thread_state.rs | noted during FUTEX-FOUND1\n")
(wt / D).write_text(s)

# ── doc/KERNEL_UNLOCKING.md: the stage record ──────────────────────────────────────────────────
U = "doc/KERNEL_UNLOCKING.md"
anchor = '''Verdict: **ENDPOINT_ROLLBACK1_ACCEPTED_WITH_BASELINE_DEBT**. Refs: `claude/yarm-kernel-unlock-u0-f2zh8p` and `wip/endpoint-rollback1`
at the commit that adds this record; `main` `b81e0317` and every other ref unchanged.
'''
patch(U, anchor, anchor + '\n' + record.rstrip('\n') + '\n')
print("docs applied")
