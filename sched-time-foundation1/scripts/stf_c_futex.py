import sys
p='src/kernel/boot/futex.rs'; s=open(p).read()
def rep(a,b):
    global s
    n=s.count(a)
    if n!=1: sys.exit(f"anchor x{n}: {a[:80]!r}")
    s=s.replace(a,b)

rep('''pub enum FutexKind {
    Private,
    Shared,
}
''','''pub enum FutexKind {
    Private,
    Shared,
    /// SCHED-TIME-FOUNDATION1 C (SLEEP1) — not a word at all: the kernel-private namespace of
    /// `ClockNanosleep` (NR 20). No ABI flag decodes to it ([`Self::from_flags`] yields only
    /// `Private` / `Shared`), it resolves no address, and the waiter it registers is on its own
    /// [`FutexKey::Sleep`] key.
    Sleep,
}
''')

rep('''    Expired {
        tid: u64,
    },
}
''','''    Expired {
        tid: u64,
    },
    /// SCHED-TIME-FOUNDATION1 C (SLEEP1) — one `ClockNanosleep` cycle of `tid`. No key resolution
    /// produces it, so no FutexWait, FutexWake, compare-and-requeue or robust exit wake can reach
    /// the sleeper; only its deadline (the timeout collector) and task exit end the cycle. Its
    /// deadline is in the MONOTONIC NANOSECOND domain (`timekeeping::monotonic_ns`), not the
    /// scheduler tick, and a sleep record is never rekeyed, so the key alone names the clock.
    Sleep {
        tid: u64,
    },
}
''')

rep('''            Self::Expired { tid } => tid.rotate_left(13) ^ 0xC2B2_AE3D_27D4_EB4F,
''','''            Self::Expired { tid } => tid.rotate_left(13) ^ 0xC2B2_AE3D_27D4_EB4F,
            Self::Sleep { tid } => tid.rotate_left(23) ^ 0x1656_67B1_9E37_79F9,
        };''' if False else '''            Self::Expired { tid } => tid.rotate_left(13) ^ 0xC2B2_AE3D_27D4_EB4F,
            Self::Sleep { tid } => tid.rotate_left(23) ^ 0x1656_67B1_9E37_79F9,
''')

# key resolution: a Sleep kind names no word
rep('''    futex_word_range_check(addr)?;
    let aspace = spaces.get(asid).ok_or(KernelError::UserMemoryFault)?;''','''    if kind == FutexKind::Sleep {
        // A sleep names no word; no caller resolves one (see `FutexKind::Sleep`).
        return Err(KernelError::WrongObject);
    }
    futex_word_range_check(addr)?;
    let aspace = spaces.get(asid).ok_or(KernelError::UserMemoryFault)?;''')
rep('''            FutexKey::Shared {
                object_id: object.id,
                offset: phys - object.phys.0,
            }
        }
    };
    Ok((key, phys))''','''            FutexKey::Shared {
                object_id: object.id,
                offset: phys - object.phys.0,
            }
        }
        FutexKind::Sleep => return Err(KernelError::WrongObject),
    };
    Ok((key, phys))''')

# timing variant
rep('''    Timed {
        deadline: u64,
        now: &'a dyn Fn() -> u64,
    },
}
''','''    Timed {
        deadline: u64,
        now: &'a dyn Fn() -> u64,
    },
    /// SCHED-TIME-FOUNDATION1 C — a `ClockNanosleep` cycle ([`FutexKind::Sleep`]) whose absolute
    /// deadline is `deadline_ns` on `CLOCK_MONOTONIC`. The clock is read INSIDE the hold
    /// ([`sleep_deadline_reached`]), so a deadline already reached publishes nothing.
    MonotonicNs { deadline_ns: u64 },
}
''')
rep('''            Self::Timed { deadline, .. } => write!(f, "Timed {{ deadline: {deadline} }}"),
''','''            Self::Timed { deadline, .. } => write!(f, "Timed {{ deadline: {deadline} }}"),
            Self::MonotonicNs { deadline_ns } => {
                write!(f, "MonotonicNs {{ deadline_ns: {deadline_ns} }}")
            }
''')

# compare-and-register: sleep branch at the top
rep('''    use FutexCompareRegisterRefusal as Refused;
    let (key, word) = match resolve_futex_word_locked(spaces, memory, asid, addr, kind) {''','''    use FutexCompareRegisterRefusal as Refused;
    if kind == FutexKind::Sleep {
        return register_sleep_waiter_locked(tcbs, table, tid, asid, timing);
    }
    let (key, word) = match resolve_futex_word_locked(spaces, memory, asid, addr, kind) {''')
rep('''    let deadline = match timing {
        FutexWaitTiming::Untimed => None,
        FutexWaitTiming::Timed { deadline, now } => {''','''    let deadline = match timing {
        FutexWaitTiming::Untimed => None,
        // Only a sleep carries a nanosecond deadline; a word wait never does.
        FutexWaitTiming::MonotonicNs { .. } => return Err(Refused::Word(KernelError::InvalidArgs)),
        FutexWaitTiming::Timed { deadline, now } => {''')

# the sleep registration owner, placed before `entering_incarnation`
rep('''/// rank 2, read-only — whether `tid` is the `Running` `{tid, asid}` incarnation the registration
/// accepts. Orders a refusal; decides nothing.''','''/// SCHED-TIME-FOUNDATION1 C — whether the sleep deadline `deadline_ns` has been reached on
/// `CLOCK_MONOTONIC`. A clock that cannot be read (no source) answers `false`: a sleep is never
/// ended EARLY because a clock is missing. Lock-free (the counter and one atomic).
pub fn sleep_deadline_reached(deadline_ns: u64) -> bool {
    crate::kernel::timekeeping::monotonic_ns().is_ok_and(|now| now >= deadline_ns)
}

/// FUTEX-TIME1 / SCHED-TIME-FOUNDATION1 C — whether `rec`'s deadline has been reached, in the
/// clock its key names: the monotonic nanosecond clock for a [`FutexKey::Sleep`] record, the
/// scheduler tick `now_tick` for every other.
fn record_deadline_reached(rec: &FutexWaiterRecord, now_tick: u64) -> bool {
    match (rec.deadline, rec.key) {
        (None, _) => false,
        (Some(d), FutexKey::Sleep { .. }) => sleep_deadline_reached(d),
        (Some(d), _) => d <= now_tick,
    }
}

/// SCHED-TIME-FOUNDATION1 C — rank 2, ONE hold: THE registration of a `ClockNanosleep` cycle.
///
/// The twin of the timed arm of [`compare_and_register_futex_waiter_locked`] with no word: the
/// incarnation, then the clock (a deadline already reached is `TimedOut` with nothing published —
/// the sleep has nothing left to wait for), then the waiter registered on its own
/// [`FutexKey::Sleep`] key through the one owner ([`register_futex_waiter_locked`]: the record and
/// `Blocked(Futex(token))` together) and its deadline attached to that exact record before the
/// hold ends ([`arm_futex_deadline_locked`], which also advances `futex_wait_generation`, the
/// coordinate the cycle's `Sleep` completion carries). Every refusal writes nothing.
fn register_sleep_waiter_locked(
    tcbs: &mut [Option<ThreadControlBlock>],
    table: &mut FutexWaiterTable,
    tid: u64,
    asid: Asid,
    timing: FutexWaitTiming<'_>,
) -> Result<(FutexKey, FutexWaitToken), FutexCompareRegisterRefusal> {
    use FutexCompareRegisterRefusal as Refused;
    let FutexWaitTiming::MonotonicNs { deadline_ns } = timing else {
        // A sleep is always timed, in nanoseconds.
        return Err(Refused::Word(KernelError::InvalidArgs));
    };
    if !entering_incarnation(tcbs, tid, asid) {
        return Err(Refused::IncarnationMoved);
    }
    if sleep_deadline_reached(deadline_ns) {
        return Err(Refused::TimedOut);
    }
    let advances = tcbs.iter().flatten().any(|t| {
        t.tid.0 == tid
            && t.asid.unwrap_or(Asid(0)) == asid
            && t.futex_wait_generation.checked_add(1).is_some()
    });
    if !advances {
        return Err(Refused::Full);
    }
    let key = FutexKey::Sleep { tid };
    let token = register_futex_waiter_locked(tcbs, table, tid, asid, key, 0).map_err(|refusal| {
        match refusal {
            FutexRegisterRefusal::IncarnationMoved => Refused::IncarnationMoved,
            FutexRegisterRefusal::Full => Refused::Full,
        }
    })?;
    if !arm_futex_deadline_locked(tcbs, table, token, deadline_ns) {
        // Unreachable (see the word arm); an untimed sleep record must never exist.
        undo_futex_registration_locked(tcbs, table, tid, token);
        return Err(Refused::Full);
    }
    Ok((key, token))
}

/// rank 2, read-only — whether `tid` is the `Running` `{tid, asid}` incarnation the registration
/// accepts. Orders a refusal; decides nothing.''')

# endings + completion
rep('''    /// The deadline: `Err(TimedOut)`.
    TimedOut,
}''','''    /// The deadline: `Err(TimedOut)`.
    TimedOut,
    /// SCHED-TIME-FOUNDATION1 C — a `ClockNanosleep` cycle's deadline: success, `ret0 = 0`.
    SleepDone,
}''')
rep('''    tcb.pending_syscall_completion = Some(BlockedSyscallCompletion {
        syscall_class: BlockedSyscallClass::FutexWait,
        result: match ending {
            FutexWaitEnding::Woken => 0,
            FutexWaitEnding::TimedOut => {
                crate::kernel::syscall::SyscallError::TimedOut.code() as u64
            }
        },''','''    tcb.pending_syscall_completion = Some(BlockedSyscallCompletion {
        syscall_class: match ending {
            FutexWaitEnding::SleepDone => BlockedSyscallClass::Sleep,
            FutexWaitEnding::Woken | FutexWaitEnding::TimedOut => BlockedSyscallClass::FutexWait,
        },
        result: match ending {
            FutexWaitEnding::Woken | FutexWaitEnding::SleepDone => 0,
            FutexWaitEnding::TimedOut => {
                crate::kernel::syscall::SyscallError::TimedOut.code() as u64
            }
        },''')

# collector
rep('''        timed_seen += 1;
        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot)
        {
            continue;
        }
        out[n] = Some(FutexTimeoutCandidate {
            tid: rec.tid,
            token: table.token(slot),
            expired: matches!(rec.key, FutexKey::Expired { .. }),
        });''','''        timed_seen += 1;
        let _ = deadline;
        if !record_deadline_reached(&rec, now)
            || matches!(rec.key, FutexKey::Pending { .. })
            || !table.live(tcbs, slot)
        {
            continue;
        }
        out[n] = Some(FutexTimeoutCandidate {
            tid: rec.tid,
            token: table.token(slot),
            // A sleep is already on its private key: its deadline is the only owner that can
            // end it, so only its placement is ever outstanding.
            expired: matches!(rec.key, FutexKey::Expired { .. } | FutexKey::Sleep { .. }),
        });''')

# claim: publish by key
rep('''    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);
    true''','''    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    let ending = if matches!(rec.key, FutexKey::Sleep { .. }) {
        FutexWaitEnding::SleepDone
    } else {
        FutexWaitEnding::TimedOut
    };
    publish_futex_wait_completion_locked(tcb, ending);
    true''')

# claimability
rep('''    rec.tid == candidate.tid
        && rec.deadline.is_some_and(|d| d <= now)
        && !matches!(rec.key, FutexKey::Pending { .. })''','''    rec.tid == candidate.tid
        && record_deadline_reached(&rec, now)
        && !matches!(rec.key, FutexKey::Pending { .. })''')
open(p,'w').write(s)
