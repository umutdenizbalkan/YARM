import sys,re
p='doc/SYSCALL_ABI.md'; s=open(p).read()
def rep(a,b):
    global s
    n=s.count(a)
    if n!=1: sys.exit(f"anchor x{n}: {a[:70]!r}")
    s=s.replace(a,b)
rep('- Public syscall count: `20` (`0..=19`)','- Public syscall count: `21` (`0..=20`)')
rep('`0..=19` (**20 slots**).','`0..=20` (**21 slots**).')
m=re.search(r'\| `19` \| `ClockSetRealtime` \| public control-plane ABI[^\n]*\n',s); assert m
s=s.replace(m.group(0), m.group(0)+'| `20` | `ClockNanosleep` | public (`arg0=clock_id`; `arg1=flags`: bit 0 `TIMER_ABSTIME`; `arg2=req_ptr`, 8-aligned `Timespec`; `arg3=rem_ptr`, `0` or 8-aligned, never written) — blocks until a `CLOCK_MONOTONIC` deadline; returns `0`. SCHED-TIME-FOUNDATION1, ABI v12 |\n',1)
m=re.search(r'\| `19` \| `ClockSetRealtime` \| public v12 \|[^\n]*\n',s); assert m
s=s.replace(m.group(0), m.group(0)+'| `20` | `ClockNanosleep` | public v12 | any user task | Sleep on a monotonic deadline; a switching class parked through the FutexWait park tail on every port. |\n',1)
rep('| `20..=22` | — | reserved gap |','| `21..=22` | — | reserved gap |')
rep('''**v11 → v12 (SCHED-TIME-FOUNDATION1).** NR `18` `ClockGetTime` and NR `19` `ClockSetRealtime`
are assigned from the reserved gap,''','''**v11 → v12 (SCHED-TIME-FOUNDATION1).** NR `18` `ClockGetTime`, NR `19` `ClockSetRealtime` and
NR `20` `ClockNanosleep` are assigned from the reserved gap,''')
rep('- assigning a reserved public gap (`20..=22` or `25`) to a user-visible public','- assigning a reserved public gap (`21..=22` or `25`) to a user-visible public')
rep('''- `20..=22`: reserved/unassigned in ABI v12 (`17` became `FutexCmpRequeue` in v11; `18` `ClockGetTime` and `19` `ClockSetRealtime` in v12).''','''- `21..=22`: reserved/unassigned in ABI v12 (`17` became `FutexCmpRequeue` in v11; `18` `ClockGetTime`, `19` `ClockSetRealtime` and `20` `ClockNanosleep` in v12).''')
rep('''- **Locking**: both syscalls are split from birth on every port and take no broad lock.''','''- **Locking**: NR 18 and NR 19 are split from birth on every port and take no broad lock.''')
sec='''
### Sleeping: `ClockNanosleep` (NR 20)

`ClockNanosleep(clock_id, flags, req_ptr, rem_ptr)` blocks the caller until `CLOCK_MONOTONIC`
reaches a deadline, then returns `0`. It is never answered before the deadline. The user runtime
offers `clock_nanosleep(clock, flags, &req, rem)` and `nanosleep(&req, rem)` (the latter is a
relative sleep on `CLOCK_MONOTONIC`).

- **Deadline.** It is fixed once, at admission, as an absolute monotonic instant in nanoseconds
  (`timekeeping::sleep_deadline_at`):
  - relative, on either clock: `now + req`. Elapsed time is not changed by a realtime adjustment.
  - absolute `CLOCK_MONOTONIC` (`TIMER_ABSTIME`): `req` itself.
  - absolute `CLOCK_REALTIME`: the monotonic instant at which realtime reaches `req`, computed
    from one coherent realtime read at admission. A later `ClockSetRealtime` does **not** move an
    admitted deadline; this kernel sets realtime once, from the control plane.
- **Realtime.** `CLOCK_REALTIME`, relative or absolute, requires an initialized realtime clock;
  otherwise the call fails with `Unavailable` and nothing is parked.
- **Zero and expired deadlines.** A zero duration, or an absolute deadline at or before now,
  returns `0` at once. Nothing is registered and the caller is not descheduled.
- **Overflow.** A deadline beyond `u64` nanoseconds saturates to `u64::MAX` (about 584 years of
  uptime). Such a sleep never ends on its own, and it never wraps into an early deadline.
- **Errors, in check order.** `InvalidArgs` for an unknown clock, an undefined flag bit, a null or
  misaligned `req_ptr`, or a misaligned `rem_ptr`. Then `PageFault` if `req_ptr` is not readable.
  Then `InvalidArgs` if `nsec` is outside `[0, 1e9)` or `sec < 0`. Then `Unavailable` if
  realtime is not initialized or there is no monotonic source. Every refusal publishes nothing.
- **No signals.** POSIX writes `rem` only when a sleep is interrupted. Nothing interrupts a sleep
  here; it ends at its deadline or with its task. So `rem_ptr` is accepted (its alignment is
  checked), it is never written, and there is no `EINTR`. A future signal contract would end the
  cycle through the same exact-token claim and fill `rem` there; this ABI reserves the slot for
  that.
- **Lifecycle.** A sleep is a blocked task with its own cycle identity. The caller is registered
  in the FUTEX-TIME1 waiter table on a kernel-private `FutexKey::Sleep { tid }` key, which no word
  resolves to. So no `FutexWait`, `FutexWake`, `FutexCmpRequeue` or robust exit wake can reach it.
  The TCB is `Blocked(Futex(token))`, the token being `{slot, generation}`, and its
  `futex_wait_generation` is advanced in the registration hold. The park is NR 9's: the same
  deferral, compare-and-clear and read-back, on both routes. The frame carries the fail-closed
  provisional (`Internal`).
- **Ending.** The bounded FUTEX-TIME1 collector is the only owner that can end a sleep.
  - It reads `CLOCK_MONOTONIC` inside its claim, so a sleep is never ended early. A missing clock
    reads as "not reached".
  - It reserves the home run-queue slot before it claims (FUTEX-WAKE-CAP1), then makes the task
    `Runnable`, retires the record and parks a `BlockedSyscallClass::Sleep` completion (success,
    `ret0 = 0`, generation-bearing) in one hold, and commits the reserved slot.
  - If the home queue is full, nothing changes: the task stays `Blocked` and its record stays
    due, and a later pass places it. A sleeper is never `Runnable` without a slot.
  - The resume boundaries consume the completion exactly where they consume a timed `FutexWait`
    one: the x86_64 completion registers and RISCV-PARK-RET1's parked-return commit included.
  - Exit retires the record, so a dead sleeper is never resurrected. A stale token claims nothing,
    and a claimed cycle cannot be claimed again.
- **Resolution and latency.**
  - The deadline is compared at the counter's resolution (`res_ns`: 1 ns at or above 1 GHz, 16 ns
    on the QEMU AArch64 counter, 100 ns on the QEMU RISC-V `time`).
  - A sleep ends at the first collector pass after its deadline. Passes run in every trap tail on
    every CPU (syscalls, timer ticks, device interrupts) and at every kernel-thread return. With
    no other activity the next pass is the next timer tick, at most one period: 0.8 s on x86_64,
    50 ms on AArch64, 10 ms on RISC-V. Placement and dispatch add scheduling latency on top. No
    wake latency is promised; only the lower bound is.
- **Timer granularity (x86_64).** The 0.8 s x86_64 period is a re-armed LAPIC count, and every
  serviced interrupt is one scheduler tick. The FutexWait and IPC tick timeouts rely on that for
  their lower bound (`n` ticks span at least `n − 1` periods). Programming an earlier one-shot for
  a sleep would add interrupts that advance the tick early and break those bounds. This stage
  therefore does not change the programming. The integration point for finer sleep wake-ups is a
  per-CPU earliest-deadline one-shot whose interrupt is not counted as a period, or tick
  accounting derived from `CLOCK_MONOTONIC`. That is a timer change outside this stage.
'''
rep('\n## Fork contract (current behavior)\n', '\n'+sec.strip('\n')+'\n\n## Fork contract (current behavior)\n')
open(p,'w').write(s)

p='scripts/check-contract-doc-enforcement.sh'; s=open(p).read()
a="""  rg -q '^\\| `19` \\| `ClockSetRealtime` \\| public' doc/SYSCALL_ABI.md
"""
assert s.count(a)==1
s=s.replace(a,a+"""  rg -q '^pub const SYSCALL_CLOCK_NANOSLEEP_NR: usize = 20;$' src/kernel/syscall.rs
  rg -q '^    pub const SYSCALL_CLOCK_NANOSLEEP_NR: usize = 20;$' crates/yarm-user-rt/src/lib.rs
  rg -q '^\\| `20` \\| `ClockNanosleep` \\| public' doc/SYSCALL_ABI.md
""")
s=s.replace("# The v12 assignment (SCHED-TIME-FOUNDATION1): NR 18 ClockGetTime / NR 19 ClockSetRealtime in","# The v12 assignment (SCHED-TIME-FOUNDATION1): NR 18 ClockGetTime / NR 19 ClockSetRealtime / NR 20 ClockNanosleep in")
open(p,'w').write(s)
