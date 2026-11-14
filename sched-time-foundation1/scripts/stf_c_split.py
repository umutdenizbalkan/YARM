import sys
p='src/kernel/syscall_split.rs'; s=open(p).read()
def rep(a,b):
    global s
    n=s.count(a)
    if n!=1: sys.exit(f"anchor x{n}: {a[:80]!r}")
    s=s.replace(a,b)

# entry: NR 20 joins NR 9
rep('''    if frame.syscall_num() != crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR {
        // THE ONLY `NotHandled` this family can produce, and it is not a fall-through: the trap is
        // a different syscall, about which this route has no opinion.
        return SplitDispatchDisposition::NotHandled;
    }
    try_split_futex_wait_recognized(shared, cpu, frame, authority).into_dispatch()''','''    // SCHED-TIME-FOUNDATION1 C: NR 20 `ClockNanosleep` parks through this same family (its own
    // front, the shared park tail).
    if frame.syscall_num() == crate::kernel::syscall::SYSCALL_CLOCK_NANOSLEEP_NR {
        return try_split_clock_nanosleep_recognized(shared, cpu, frame, authority).into_dispatch();
    }
    if frame.syscall_num() != crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR {
        // THE ONLY `NotHandled` this family can produce, and it is not a fall-through: the trap is
        // a different syscall, about which this route has no opinion.
        return SplitDispatchDisposition::NotHandled;
    }
    try_split_futex_wait_recognized(shared, cpu, frame, authority).into_dispatch()''')

# split the body at step (6)
rep('''    // (6) ADMISSION, on the trap's own authority.
''','''    split_futex_park_tail(
        shared, cpu, frame, authority, tid, addr, expected, args.kind, deadline,
    )
}

/// SCHED-TIME-FOUNDATION1 C — the NR 20 front: the decode, the caller, the request's copy-in and
/// its deadline (`syscall::clock`, the broad handler's own functions, in its order), then THE park
/// tail NR 9 uses, on the caller's kernel-private `FutexKey::Sleep` key. Every refusal before the
/// tail is pre-mutation and answered here.
#[cfg(not(feature = "hosted-dev"))]
fn try_split_clock_nanosleep_recognized(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
) -> SplitBlockingDisposition {
    use crate::kernel::syscall::SyscallError;
    use crate::kernel::syscall::clock as ck;
    use SplitBlockingDisposition as D;
    if cpu.0 as usize >= crate::kernel::scheduler::MAX_CPUS {
        crate::yarm_log!(
            "CLOCK_NANOSLEEP_SPLIT_INVARIANT cpu={} reason=cpu_out_of_range result=failed_closed",
            cpu.0
        );
        return D::Complete(Err(TrapHandleError::Syscall(SyscallError::WrongObject)));
    }
    let done = |tid: u64, why: &str, err: SyscallError| {
        crate::yarm_log!(
            "CLOCK_NANOSLEEP_SPLIT_DONE tid={} result={} err={:?} cpu={}",
            tid,
            why,
            err,
            cpu.0
        );
        D::Complete(Err(TrapHandleError::Syscall(err)))
    };
    let req = match ck::decode_clock_nanosleep(frame) {
        Ok(req) => req,
        Err(err) => return done(0, "invalid_args", err),
    };
    let Some(tid) = shared.current_tid_authoritative(cpu) else {
        return done(
            0,
            "no_current_task",
            SyscallError::from(crate::kernel::boot::KernelError::TaskMissing),
        );
    };
    let asid = shared.task_asid_for_tid_split_read(tid);
    if asid == 0 {
        return done(
            tid,
            "no_address_space",
            SyscallError::from(crate::kernel::boot::KernelError::UserMemoryFault),
        );
    }
    let raw = match shared.copy_from_user_split(
        crate::kernel::vm::Asid(asid as u16),
        req.ptr,
        crate::kernel::timekeeping::TIMESPEC_SIZE,
    ) {
        Ok(raw) => raw,
        Err(err) => return done(tid, "request_unreadable", SyscallError::from(err)),
    };
    let deadline = match ck::nanosleep_deadline(req, &raw) {
        Ok(deadline) => deadline,
        Err(err) => return done(tid, "request_refused", err),
    };
    split_futex_park_tail(
        shared,
        cpu,
        frame,
        authority,
        tid,
        0,
        0,
        crate::kernel::boot::futex::FutexKind::Sleep,
        Some(deadline),
    )
}

/// U9-FUTEX-WAIT-FINAL §2 / SCHED-TIME-FOUNDATION1 C — THE switching park tail of the FutexWait
/// family, from admission on: NR 9 once its word is known to match, NR 20 (`kind ==
/// FutexKind::Sleep`, `addr == 0`) once its deadline is known. Every outcome is settled here.
#[cfg(not(feature = "hosted-dev"))]
#[allow(clippy::too_many_arguments)]
fn split_futex_park_tail(
    shared: &SharedKernel,
    cpu: CpuId,
    frame: &mut TrapFrame,
    authority: crate::runtime::DispatchAuthority,
    tid: u64,
    addr: usize,
    expected: u32,
    kind: crate::kernel::boot::futex::FutexKind,
    deadline: Option<u64>,
) -> SplitBlockingDisposition {
    use crate::kernel::syscall::SyscallError;
    use crate::kernel::syscall::sched::FutexParkOutcome;
    use SplitBlockingDisposition as D;
    let cpu_idx = cpu.0 as usize;
    let sleep = kind == crate::kernel::boot::futex::FutexKind::Sleep;

    // (6) ADMISSION, on the trap's own authority.
''')
rep('''    let outcome = shared
        .futex_wait_park_exact_timed_split(cpu, tid, asid, addr, expected, args.kind, deadline);''','''    let outcome =
        shared.futex_wait_park_exact_timed_split(cpu, tid, asid, addr, expected, kind, deadline);''')
rep('''                        return D::Complete(Err(TrapHandleError::Syscall(
                            SyscallError::from_code(done.result as usize),
                        )));
                    }
                    _ => frame.set_ok(1, 0, 0),
                }''','''                        return D::Complete(Err(TrapHandleError::Syscall(
                            SyscallError::from_code(done.result as usize),
                        )));
                    }
                    // The class decides the success value: `1` for a woken wait, `0` for a sleep
                    // (SCHED-TIME-FOUNDATION1 C).
                    Some(done) => frame.set_ok(done.success_ret0(), 0, 0),
                    None => frame.set_ok(1, 0, 0),
                }''')
rep('''        FutexParkOutcome::TimedOut => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=timed_out_at_admission cpu={}",
                tid,
                addr,
                cpu.0
            );
            D::Complete(Err(TrapHandleError::Syscall(SyscallError::TimedOut)))
        }''','''        FutexParkOutcome::TimedOut => {
            crate::kernel::boot::futex_wait_dispatch_clear(cpu_idx);
            crate::yarm_log!(
                "FUTEX_WAIT_SPLIT_DONE tid={} addr={} result=timed_out_at_admission cpu={} sleep={}",
                tid,
                addr,
                cpu.0,
                u8::from(sleep)
            );
            // SCHED-TIME-FOUNDATION1 C: a sleep whose deadline had been reached has nothing
            // left to wait for — success, `0`.
            if sleep {
                frame.set_ok(0, 0, 0);
                return D::Complete(Ok(()));
            }
            D::Complete(Err(TrapHandleError::Syscall(SyscallError::TimedOut)))
        }''')
open(p,'w').write(s)
