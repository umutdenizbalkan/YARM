import sys
p='src/kernel/task.rs'; s=open(p).read()
def rep(a,b):
    global s
    n=s.count(a)
    if n!=1: sys.exit(f"anchor x{n}: {a[:80]!r}")
    s=s.replace(a,b)
rep('''    /// real one here. Validated against `futex_wait_generation`.
    FutexWait,
}''','''    /// real one here. Validated against `futex_wait_generation`.
    FutexWait,
    /// SCHED-TIME-FOUNDATION1 C — a `ClockNanosleep` (NR 20) cycle. Like a timed `FutexWait` its
    /// parked frame carries a fail-closed provisional, and its one terminal owner (the deadline)
    /// parks the result here: success, `ret0 = 0`. It shares the futex waiter table, so it is
    /// validated against the same `futex_wait_generation`, and it is consumed by every boundary
    /// that consumes a `FutexWait` completion (one blocked cycle at a time: never both).
    Sleep,
}''')
rep('''            Self::FutexWait => "futex_wait",
        }''','''            Self::FutexWait => "futex_wait",
            Self::Sleep => "sleep",
        }''')
rep('''            BlockedSyscallClass::FutexWait => 1,
            BlockedSyscallClass::IpcRecv | BlockedSyscallClass::IpcSend => 0,''','''            BlockedSyscallClass::FutexWait => 1,
            BlockedSyscallClass::IpcRecv
            | BlockedSyscallClass::IpcSend
            | BlockedSyscallClass::Sleep => 0,''')
rep('''            BlockedSyscallClass::IpcSend | BlockedSyscallClass::FutexWait
        )''','''            BlockedSyscallClass::IpcSend
                | BlockedSyscallClass::FutexWait
                | BlockedSyscallClass::Sleep
        )''')
rep('''        BlockedSyscallClass::FutexWait => true,
        BlockedSyscallClass::IpcRecv => {''','''        BlockedSyscallClass::FutexWait => true,
        // SCHED-TIME-FOUNDATION1 C: a sleep's result is consumed exactly where a timed wait's is.
        BlockedSyscallClass::Sleep => true,
        BlockedSyscallClass::IpcRecv => {''')
rep('''    let send_completion = send_completion.or_else(|| {
        take_blocked_syscall_completion_for(tcbs, tid, expect_asid, BlockedSyscallClass::FutexWait)
    });''','''    let send_completion = send_completion
        .or_else(|| {
            take_blocked_syscall_completion_for(
                tcbs,
                tid,
                expect_asid,
                BlockedSyscallClass::FutexWait,
            )
        })
        .or_else(|| {
            take_blocked_syscall_completion_for(tcbs, tid, expect_asid, BlockedSyscallClass::Sleep)
        });''')
rep('''            BlockedSyscallClass::FutexWait => tcb.futex_wait_generation,
        };
        self.tid == tcb.tid.0''','''            BlockedSyscallClass::FutexWait | BlockedSyscallClass::Sleep => {
                tcb.futex_wait_generation
            }
        };
        self.tid == tcb.tid.0''')
open(p,'w').write(s)

p='src/kernel/boot/ipc_state.rs'; s=open(p).read()
rep('''        self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::IpcSend)
            .or_else(|| {
                self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::FutexWait)
            })''','''        self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::IpcSend)
            .or_else(|| {
                self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::FutexWait)
            })
            .or_else(|| {
                self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::Sleep)
            })''')
rep('''                    crate::kernel::task::BlockedSyscallClass::FutexWait => {
                        tcb.futex_wait_generation
                    }''','''                    crate::kernel::task::BlockedSyscallClass::FutexWait
                    | crate::kernel::task::BlockedSyscallClass::Sleep => tcb.futex_wait_generation,''')
open(p,'w').write(s)

p='src/arch/x86_64/trap.rs'; s=open(p).read()
rep('''    if done.syscall_class == crate::kernel::task::BlockedSyscallClass::FutexWait {
        frame.commit_x86_syscall_returns();
    }''','''    // SCHED-TIME-FOUNDATION1 C: a sleep completion resumes on the same boundaries, from the
    // same provisional snapshot, so its result goes into the registers too.
    if matches!(
        done.syscall_class,
        crate::kernel::task::BlockedSyscallClass::FutexWait
            | crate::kernel::task::BlockedSyscallClass::Sleep
    ) {
        frame.commit_x86_syscall_returns();
    }''')
open(p,'w').write(s)

p='src/runtime.rs'; s=open(p).read()
rep('''        self.with_task_tcbs_split_mut(|tcbs| {
            crate::kernel::task::take_blocked_syscall_completion_for(
                tcbs,
                tid,
                Some(asid),
                crate::kernel::task::BlockedSyscallClass::FutexWait,
            )
        })''','''        self.with_task_tcbs_split_mut(|tcbs| {
            use crate::kernel::task::{BlockedSyscallClass, take_blocked_syscall_completion_for};
            // SCHED-TIME-FOUNDATION1 C: a sleep cycle's completion is the same kind of record.
            take_blocked_syscall_completion_for(
                tcbs,
                tid,
                Some(asid),
                BlockedSyscallClass::FutexWait,
            )
            .or_else(|| {
                take_blocked_syscall_completion_for(
                    tcbs,
                    tid,
                    Some(asid),
                    BlockedSyscallClass::Sleep,
                )
            })
        })''')
open(p,'w').write(s)
