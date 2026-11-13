# FUTEX-TIME1 — source audit and design (at 6ba03af9)

## 1. Ownership graph today

| object | owner / lock | written by | identity |
|---|---|---|---|
| `FutexWaiterTable` (512 records, 64 FIFO buckets, per-slot generation) | task domain, rank 2, beside `tcbs` | register (compare-and-register, ranks 2→5→6 one hold), claim (wake / pending-wake), undo (own publication), rekey (NR 17 commit, ranks 2→5→6), retire (claim, undo, stale sweeps, task death in `restart_state.rs`) | `FutexWaitToken {slot, generation}`; record also carries `tid`, `tcb` slot |
| TCB status `Blocked(Futex(token))` | rank 2 | same critical sections as the record | the token |
| run-queue slot reservation | per-CPU 1L, rank 1 | `reserve_enqueue_on_cpu_locked` / `release_…` / `commit_reserved_…` | count per (cpu, priority); grants NO ownership of a waiter |
| `pending_syscall_completion` | rank 2 | U6 send claim, recv/reply timeouts, server death | `{class, tid, asid, class generation}`; taken by the resume boundary (IpcSend on every port) |
| scheduler clock | `SchedDomain.clock: AtomicU64`, single writer `CpuView::tick` on `CLOCK_OWNER_CPU` under its 1L | timer interrupts only (split timer route, kthread timer service) | — |
| IPC deadlines | `tcb.ipc_timeout_deadline` (rank 2), collected by `collect_due_ipc_timeout_work` (task-lock, cursor-bounded) and settled by per-class drains | — | per-class generations |

Terminal owners of a futex wait cycle today: a wake claim (NR 10 / robust exit wake), the NR 17
wake portion (commit → `Pending{op,cpu,prio}` key → claim), the waiter's own undo (victim changed),
task death (retire). Requeue without wake is a rekey (non-terminal; token unchanged).

Wake protocol (FUTEX-WAKE-CAP1): rank 2 candidate+plan → rank 1 reserve → rank 2 exact claim
(`Runnable` + retire in one hold) → rank 1 reserved commit (cannot be refused for capacity) →
`notify_enqueued`. Never nested; rank 2 never holds rank 1.

Result delivery today: the split park writes `set_ok(1,0,0)` into the outgoing frame before the
switch (`QueueAdvanceCommitted`); RISC-V additionally commits it into the return lanes
(`riscv_commit_final_park_result`); the resume restores the captured context. Correct only because
an untimed parked wait has one possible outcome.

Pre-existing, not inherited: the IPC timeout drains (`drain_recv_timeout_post_work`,
`drain_send_timeout_post_work`) set `Runnable` and then call a FALLIBLE
`enqueue_reply_timeout_wake_split`, whose refusal is discarded — a full home queue strands a
Runnable-but-unqueued task. Recorded as an observation; futex timeouts do not use this path.

## 2. Clock audit (production)

| question | x86_64 PVH | AArch64 virt | RISC-V virt |
|---|---|---|---|
| 1. what advances the tick | LAPIC timer IRQ on CPU 0 (`settle_recognized_timer` → `scheduler_tick_split_mut` → `CpuView::tick`, owner = `CLOCK_OWNER_CPU` = 0) | CNTP (EL1 physical timer) PPI on CPU 0, same route | S-mode timer (SBI `set_timer`) on hart 0, same route |
| 2. periodic | yes: one-shot re-armed at every serviced tick with `BOOTSTRAP_TIMER_DEADLINE_TICKS` = 50e6 @ 62.5 MHz → **0.8 s** | yes: `CNTP_TVAL` 3_125_000 @ 62.5 MHz → **50 ms** | yes: `rdtime` + 100_000 @ 10 MHz → **10 ms** (re-arm from a fresh sample) |
| 3. continues with all user tasks blocked | yes: idle `hlt` with IF=1 takes the LAPIC timer (`TIMER_SPLIT_IDLE_ADVANCE_COMMITTED cpu=0 tick=7..71` in a default SMP2 boot) | yes: idle `wfi` takes CNTP (`IDLE_ADVANCE … cpu=0 tick=23..36`) | yes: idle `wfi` with `sstatus.SIE` set at the audited idle boundary (`IDLE_ADVANCE … cpu=0 tick=325..1302`) |
| 4. deadline collection CPU | any CPU's post-lock trap area (the IPC pipeline runs there on every trap); CPU 0 is guaranteed one per period | same | same (`handle_riscv_trap_entry_shared`, which the idle timer path also enters) |
| 5. lock-free read | `SchedDomain::clock_now()` — one Acquire load | same | same |
| 6. monotonic across CPUs | single writer, `fetch_add` AcqRel / Acquire loads: never decreases on any CPU | same | same |
| 7. interaction with preemption | the same interrupt also consumes the per-CPU quantum; the tick is taken whether or not the tick preempts | same | same |
| 8. masked sections | delay delivery; no catch-up — one interrupt per re-arm, so the clock counts SERVICED periods and lags wall time while CPU 0 is masked | same | same (pending STIP is taken at the next `sret`/idle) |
| 9. idle keeps a wake source | yes (CPU 0's own periodic timer) | yes | yes |
| 10. can the scanner miss an expired waiter forever | not if it runs on every CPU-0 timer trap with a cursor that visits every slot (see §5) | same | same |

No hard stop: the production tick advances on all three ports, idle included. Its resolution is the
per-port period above, which is why the public unit is NOT the tick.

## 3. ABI (selected)

* NR 9 `FutexWait(addr, expected, flags, timeout_ns)`. Flag bit 0 `FUTEX_FLAG_SHARED`, **bit 1
  `FUTEX_FLAG_TIMED`** (new). `args[3]` is read only when TIMED is set: a RELATIVE duration in
  nanoseconds (u64). NR 10 keeps rejecting bit 1; NR 17 unchanged. No syscall number changes.
* Why relative nanoseconds: no syscall exposes the scheduler tick (an absolute deadline would name
  an unqueryable domain), and the tick period differs per port (0.8 s / 50 ms / 10 ms), so a
  tick-valued ABI would mean different durations on each ISA.
* Conversion, once, at admission: `0` → deadline = now (already due); `d > 0` → `ticks =
  ceil(d / P) + 1`, deadline = `now + ticks` by `checked_add` (overflow → `InvalidArgs`; unreachable:
  2^64 ticks ≥ 5.8e9 years). Guarantee: at least `d` elapses (each increment needs one full re-armed
  period; the `+1` absorbs the partial current period). No upper bound (masked sections, latency).
* Results: `0` mismatch (never registered, even if already expired) · `1` a wake won ·
  `Err(TimedOut)` (9) the timeout won · existing errors unchanged. Order: decode (`InvalidArgs`) →
  range → task → compare (inside the 2→5→6 hold) → expiry (inside the same hold, after the compare)
  → register WITH the deadline in the same hold.
* `SYSCALL_ABI_VERSION` stays 11: the versioning policy reserves a bump for breaking changes; a
  previously reserved flag bit becoming defined is additive (a v11 binary that never sets it is
  unaffected; one that sets it was refused `InvalidArgs` before).

## 4. Deadline state

On the existing record: `deadline: Option<u64>` (tick domain). Lifetime = the wait cycle (slot +
generation); rekey (requeue) never touches it; retire frees it with the record; a reused slot has a
new generation and a fresh record. A timed record is in one of:

* `Waiting` — on its user key (or a target key after requeue): wake, NR 17 and timeout may claim.
* `Pending{op,…}` — selected by an NR 17 wake portion: only that operation and exit; timeout skips.
* `Expired{tid}` (new private key) — the timeout has WON but its home queue could not take it yet:
  no key resolution reaches it, so no wake/requeue can; only the collector's placement retry and
  exit. The TCB stays `Blocked(Futex(token))` (never Runnable-unqueued).

Terminal = the claim (`Runnable` + retire + completion published, one rank-2 hold). Exactly one:
every claimer requires `token` still the slot's generation and the TCB still `Blocked(Futex(token))`.

## 5. Capacity policy: Option B with the existing record

Collector (bounded, per post-lock trap on every CPU, skipped when no timed record exists):
rank 2 scan of ≤ `FUTEX_TIMEOUT_SCAN_WINDOW` slots from the table's cursor, ≤
`FUTEX_TIMEOUT_DUE_MAX` due candidates; cursor advances to just past the last slot examined (a
refused candidate is revisited next cycle — every slot once per `512 / window` passes). Per
candidate: plan (rank 2) → reserve (rank 1) → exact timeout claim (rank 2) → reserved commit
(rank 1) → notify. Reservation refused → rank 2 rekey `Waiting → Expired` (the timeout wins now;
placement retried by later scans); a wake in that window finds no record on its key.

## 6. Result delivery on all three ports

New completion class `BlockedSyscallClass::FutexWait` with its own TCB generation
`futex_wait_generation` (advanced by `checked_add` at each TIMED registration). Every timed terminal
claim publishes one: a wake claim → success (`ret0 = 1`), the timeout claim → `TimedOut`. The
resume boundaries that consume `IpcSend` (x86_64 exact-token + owner revalidation, AArch64
exact-token + facts, RISC-V exact-token + in-lock) consume `FutexWait` with the same identity rule.
The parked frame of a TIMED wait carries a FAIL-CLOSED provisional (`Err(Internal)`), never `1`, so
a resume that consumed nothing could never report success; RISC-V commits that provisional, never
`1`. A claim that lands during publication is settled in place from the completion (taken there).
