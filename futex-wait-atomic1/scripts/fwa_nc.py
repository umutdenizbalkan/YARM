#!/usr/bin/env python3
"""FUTEX-WAIT-ATOMIC1 negative controls. Usage: fwa_nc.py <worktree> <ncN>
Each control reintroduces ONE defect class into the candidate; the permanent tests must catch it
behaviourally (a compile failure, or a source-text guard alone, does not count)."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
F = "src/kernel/boot/futex.rs"; R = "src/runtime.rs"

def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))

def between(rel, start, end, new):
    """Replace the text from `start` (inclusive) up to `end` (exclusive), both unique."""
    p = wt / rel; s = p.read_text()
    assert s.count(start) == 1 and s.count(end) == 1, (rel, start[:60], end[:60])
    i = s.index(start); j = s.index(end)
    assert i < j
    p.write_text(s[:i] + new + s[j:])

TOP = '''        #[cfg(test)]
        fx::futex_phase_hook::run_before_register();
        // FUTEX-WAIT-ATOMIC1 live witness (default off): window B, before the compare-and-register.'''
TXN_END = '''        let token = match registered {'''
TXN_CALL = '''                fx::compare_and_register_futex_waiter_locked(
                    tcbs, table, spaces, memory, tid, asid, addr, expected, kind,
                )'''

def nc1():
    # The separated protocol restored: read and compare under vm -> memory, release, then register
    # under the task lock alone. The test hook and the live witness's window B sit in that gap, as
    # the old interval did, so this one patch is the hosted control and the live control alike.
    between(R, TOP, TXN_END, '''        #[cfg(feature = "futex-wait-witness")]
        let witness = fx::wait_witness::begin(expected).filter(|&w| {
            let peer = self.futex_wait_witness_peer_running_split(cpu, tid, asid);
            if !peer {
                fx::wait_witness::skip(w, cpu.0, tid);
            }
            peer
        });
        let registered = match self.resolve_futex_word_in_split(asid, addr, kind) {
            Err(e) => Err(FutexCompareRegisterRefusal::Word(e)),
            Ok((_, w)) if w != expected => Err(FutexCompareRegisterRefusal::ValueChanged),
            Ok((key, _)) => {
                #[cfg(test)]
                fx::futex_phase_hook::run_before_register();
                #[cfg(feature = "futex-wait-witness")]
                if let Some(w) = witness {
                    let held = fx::wait_witness::hold('B', w, cpu.0, tid);
                    let word = self.futex_wait_witness_word_split(asid, addr, kind);
                    fx::wait_witness::close('B', w, held, cpu.0, expected, word);
                }
                self.with_task_futex_split_mut(|tcbs, table| {
                    fx::register_futex_waiter_locked(tcbs, table, tid, asid, key, addr as u64)
                })
                .map(|t| (key, t))
                .map_err(|_| FutexCompareRegisterRefusal::IncarnationMoved)
            }
        };
''')

def nc2():
    # The comparison removed: every wait registers, whatever its word.
    patch(F, '''    if !crate::kernel::syscall::sched::futex_should_park(expected, word) {
        return Err(Refused::ValueChanged);
    }
    register_futex_waiter_locked(''', '''    let _ = (expected, word);
    register_futex_waiter_locked(''')

def nc3():
    # A stale key: the identity is resolved before the hold (as of the decision) and the waiter is
    # registered on it, although the word is compared inside the hold.
    patch(R, TOP, '''        let early = self.resolve_futex_word_in_split(asid, addr, kind).map(|(k, _)| k);
''' + TOP)
    patch(R, TXN_CALL, '''                match fx::resolve_futex_word_locked(spaces, memory, asid, addr, kind) {
                    Err(e) => Err(FutexCompareRegisterRefusal::Word(e)),
                    Ok((_, w)) if w != expected => Err(FutexCompareRegisterRefusal::ValueChanged),
                    Ok((fresh, _)) => {
                        let key = early.clone().unwrap_or(fresh);
                        fx::register_futex_waiter_locked(tcbs, table, tid, asid, key, addr as u64)
                            .map(|t| (key, t))
                            .map_err(|_| FutexCompareRegisterRefusal::IncarnationMoved)
                    }
                }''')

def nc4():
    # A second read that retracts the registration on a changed word, unconditionally — even when a
    # requeue has already moved (or a wake taken) the waiter.
    patch(R, '''        #[cfg(test)]
        fx::futex_phase_hook::run_after_register();
''', '''        #[cfg(test)]
        fx::futex_phase_hook::run_after_register();
        if !matches!(self.resolve_futex_word_in_split(asid, addr, kind), Ok((_, w)) if w == expected)
            && undo_registration()
        {
            return O::ValueChanged;
        }
''')

def nc5():
    # The undo ignores the exact token: any futex wait of the task is undone.
    patch(F, '''        .find(|t| t.tid.0 == tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token)))
    else {
        return false;
    };
    tcb.status = TaskStatus::Running;''', '''        .find(|t| t.tid.0 == tid && matches!(t.status, TaskStatus::Blocked(WaitReason::Futex(_))))
    else {
        return false;
    };
    tcb.status = TaskStatus::Running;''')

def nc6():
    # A wake that races the current-slot park is not recovered: the read-back is ignored.
    patch(R, '''        let still_parked = self.with_task_futex_split_mut(|tcbs, _table| {
            fx::futex_still_parked_locked(tcbs, tid, token)
        });''', '''        let still_parked = self.with_task_futex_split_mut(|tcbs, _table| {
            let _ = fx::futex_still_parked_locked(tcbs, tid, token);
            true
        });''')

def nc7():
    # The TCB status changes without a record being published (the record write and its link are
    # dropped; the token is still taken from the slot's generation).
    patch(F, '''    table.records[slot] = Some(FutexWaiterRecord {
        key,
        tid,
        tcb: tcb_idx as u16,
        va,
        next: NIL,
    });
    table.link_tail(slot);
    let token = table.token(slot);''', '''    let _ = (key, va);
    let token = table.token(slot);''')

def nc8():
    # A record is published without the exact TCB wait status (the status written by the one
    # registration line is overwritten back to Running).
    patch(F, '''    if let Some(tcb) = tcbs[tcb_idx].as_mut() {
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));
    }
    Ok(token)''', '''    if let Some(tcb) = tcbs[tcb_idx].as_mut() {
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));
    }
    if let Some(tcb) = tcbs[tcb_idx].as_mut() {
        tcb.status = TaskStatus::Running;
    }
    Ok(token)''')

def nc9():
    # The private key reuses the prior address-space incarnation (no generation at all).
    patch(F, '''        FutexKind::Private => FutexKey::Private {
            asid,
            aspace_generation,
            va: addr as u64,
        },''', '''        FutexKind::Private => FutexKey::Private {
            asid,
            aspace_generation: {
                let _ = aspace_generation;
                0
            },
            va: addr as u64,
        },''')

def nc10():
    # A wake claims its waiter before its placement is secured: when the reservation is refused the
    # waiter has already been taken.
    patch(R, '''            if let Err(err) = reserved {
                crate::yarm_log!(
                    "FUTEX_WAKE_STOPPED tid={} cpu={} reason=no_placement err={:?} woke={}",''', '''            if let Err(err) = reserved {
                let _ = self.with_task_futex_split_mut(|tcbs, table| {
                    fx::claim_futex_waiter_locked(tcbs, table, key, candidate)
                });
                crate::yarm_log!(
                    "FUTEX_WAKE_STOPPED tid={} cpu={} reason=no_placement err={:?} woke={}",''')

def nc11():
    # A record stays live after its task exits: exit no longer retires it, and liveness accepts an
    # exited TCB that still names it.
    patch(F, '''        if table.records[slot].is_some_and(|r| r.tid == tid) {
            table.retire(slot);
            n += 1;
        }''', '''        if table.records[slot].is_some_and(|r| r.tid == tid) {
            n += 1;
        }''')
    patch(F, '''                t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
            })
    }''', '''                t.tid.0 == rec.tid
                    && (t.status == TaskStatus::Blocked(WaitReason::Futex(token))
                        || matches!(t.status, TaskStatus::Exited(_)))
            })
    }''')

C = {f"nc{i}": globals()[f"nc{i}"] for i in range(1, 12)}
C[which]()
print(f"{which} applied")
