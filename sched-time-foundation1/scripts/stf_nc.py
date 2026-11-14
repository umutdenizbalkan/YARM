#!/usr/bin/env python3
"""SCHED-TIME-FOUNDATION1 negative controls. Usage: stf_nc.py <worktree> <ncId>
Each control reintroduces ONE defect into the candidate; the permanent hosted tests must catch it
by behaviour (a compile failure does not count)."""
import pathlib
import sys

wt = pathlib.Path(sys.argv[1])
which = sys.argv[2]
SCH = "src/kernel/scheduler.rs"
RT = "src/runtime.rs"
IPC = "src/kernel/boot/ipc_state.rs"


def patch(rel, old, new, count=1):
    p = wt / rel
    s = p.read_text()
    assert s.count(old) == count, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))


# ── Checkpoint A: scheduler ────────────────────────────────────────────────────────────────
def ncA1():
    # Restore full-queue rotation refusal: the returning occupant goes through admission again.
    patch(SCH, """        let pushed = self.push_unchecked_capacity(tid, priority);
        // The one current""", """        let pushed = self.enqueue_with_priority(tid, priority);
        // The one current""")


def ncA2():
    # Duplicate the current task during the return: no duplicate refusal.
    patch(SCH, """    ) -> Result<(), SchedulerError> {
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }
        let pushed = self.push_unchecked_capacity(tid, priority);""", """    ) -> Result<(), SchedulerError> {
        let _ = self.push_unchecked_capacity(tid, priority);
        let pushed = self.push_unchecked_capacity(tid, priority);""")


def ncA3():
    # Corrupt priority ordering: the returning task is queued at High whatever its class.
    patch(SCH, """        let pushed = self.push_unchecked_capacity(tid, priority);
        // The one current""", """        let _ = priority;
        let pushed = self.push_unchecked_capacity(tid, TaskPriority::High);
        // The one current""")


def ncA4():
    # Lose the selected task during rollback: `preempt_reenqueue_only` clears current but never
    # returns it to a queue.
    patch(SCH, """        if let Err(_err) = self.requeue_current_with_priority(running.tid, running.priority) {""",
          """        if let Err(_err) = Ok::<(), SchedulerError>(()) {""")


def ncA5():
    # Ignore a reserved queue slot: admission no longer counts reservations.
    patch(SCH, """        self.queues[p].len + self.reserved[p] < MAX_RUN_QUEUE""",
          """        self.queues[p].len < MAX_RUN_QUEUE""")


# ── Checkpoint A: IPC timeout ──────────────────────────────────────────────────────────────
def ncA6():
    # Mark Runnable before guaranteed placement: the receive drain claims without a promise and
    # enqueues afterwards, discarding QueueFull (the original defect).
    patch(RT, """            let Some(placement) = self.reserve_ipc_wake_split(work.tid, cpu) else {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid={} wait_generation={} cpu={}",""",
          """            let placement = self.reserve_ipc_wake_split(work.tid, cpu);
            let Some(placement) = Some(placement) else {
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_PLACEMENT_DEFERRED tid={} wait_generation={} cpu={}",""")
    patch(RT, """            if !claimed {
                self.release_ipc_wake_split(placement);
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_REFUSED_STALE tid={} asid={} wait_generation={} deadline={}",""",
          """            if !claimed {
                if let Some(p) = placement {
                    self.release_ipc_wake_split(p);
                }
                crate::yarm_log!(
                    "U8_RECV_TIMEOUT_REFUSED_STALE tid={} asid={} wait_generation={} deadline={}",""")
    patch(RT, """            // ── (3) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            self.commit_ipc_wake_split(placement);""",
          """            // ── (3) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            if let Some(p) = placement {
                self.commit_ipc_wake_split(p);
            }""")


def ncA7():
    # Discard a full-queue refusal in the send drain: claim even when no slot was promised.
    patch(RT, """            let Some(placement) = self.reserve_ipc_wake_split(work.tid, cpu) else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_PLACEMENT_DEFERRED tid={} send_generation={} cpu={}",""",
          """            let placement = self.reserve_ipc_wake_split(work.tid, cpu);
            let Some(placement) = Some(placement) else {
                crate::yarm_log!(
                    "U7_SEND_TIMEOUT_PLACEMENT_DEFERRED tid={} send_generation={} cpu={}",""")
    patch(RT, """            let Some(envelope) = withdrawn else {
                self.release_ipc_wake_split(placement);""", """            let Some(envelope) = withdrawn else {
                if let Some(p) = placement {
                    self.release_ipc_wake_split(p);
                }""")
    patch(RT, """            if !claimed {
                self.release_ipc_wake_split(placement);
                continue;
            }
            // ── (4) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            self.commit_ipc_wake_split(placement);""", """            if !claimed {
                if let Some(p) = placement {
                    self.release_ipc_wake_split(p);
                }
                continue;
            }
            // ── (4) rank 1: the wake, last — the promised slot, which cannot be refused ─────
            if let Some(p) = placement {
                self.commit_ipc_wake_split(p);
            }""")


def ncA8():
    # Allow timeout and reply both to complete: the timeout ignores a terminal another owner won.
    patch(IPC, """    let Some(terminal_owner) = rt_try_claim_timeout_terminal(d, record_index, &identity) else {
        rt_disarm_deadline_fire(d, token_index, &fire_owner);
        d.rtd_release(placement);
        return ReplyTimeoutOutcome::LostToTerminal;
    };""", """    let terminal_owner = match rt_try_claim_timeout_terminal(d, record_index, &identity) {
        Some(owner) => owner,
        None => {
            let _ = &fire_owner;
            let _ = rt_prepare_timeout_result(d, identity.caller_tid.0, identity.caller_asid);
            let _ = rt_commit_receiver_runnable(
                d,
                identity.caller_tid.0,
                identity.caller_asid,
                crate::kernel::syscall::SyscallError::TimedOut as u64,
            );
            d.rtd_enqueue_reserved(placement);
            return ReplyTimeoutOutcome::LostToTerminal;
        }
    };""")


def ncA9():
    # Reuse a stale timeout generation: the receive claim no longer checks the wait generation.
    patch(RT, """                if tcb.blocked_recv_generation != work.wait_generation {
                    return false;
                }""", """                let _ = work.wait_generation;""")
    patch(RT, """                if deadline != work.deadline || now < deadline {
                    return false;
                }""", """                let _ = deadline;""")


def ncA10():
    # Lose a pending placement permanently: a deferred server death is dropped, not re-parked.
    patch(RT, """                        Some(slot @ None) => {
                            *slot = Some(crate::kernel::boot::ParkedServerDeath {
                                work,
                                claimable: true,
                            });
                            crate::kernel::boot::server_death_claimable_note(
                                work.reply_record_index,
                            );
                            true
                        }""", """                        Some(slot @ None) => {
                            let _ = slot;
                            true
                        }""")



# ── Checkpoint B: timekeeping ──────────────────────────────────────────────────────────────
TK = "src/kernel/timekeeping.rs"
CK = "src/kernel/syscall/clock.rs"
UM = "src/kernel/boot/user_memory_state.rs"


def ncB1():
    # Wrong frequency: the conversion uses microseconds per second.
    patch(TK, """    u64::try_from(delta as u128 * NSEC_PER_SEC as u128 / freq_hz as u128).ok()""",
          """    u64::try_from(delta as u128 * 1_000_000u128 / freq_hz as u128).ok()""")


def ncB2():
    # Decreasing monotonic across CPUs: the clamp of an unsynchronized source is removed.
    patch(TK, """        let prev = LAST_MONOTONIC.fetch_max(ns, Ordering::AcqRel);
        Ok(ns.max(prev))""", """        let _ = LAST_MONOTONIC.fetch_max(ns, Ordering::AcqRel);
        Ok(ns)""")


def ncB3():
    # Overflow wraps instead of being refused.
    patch(TK, """    u64::try_from(delta as u128 * NSEC_PER_SEC as u128 / freq_hz as u128).ok()""",
          """    Some((delta as u128 * NSEC_PER_SEC as u128 / freq_hz as u128) as u64)""")


def ncB4():
    # Uninitialized realtime exposed as valid (an invented epoch at the boot counter).
    patch(TK, """        Some((false, _)) => Err(ClockError::Unavailable),""",
          """        Some((false, off)) => {
            Timespec::from_signed_ns(mono as i128 + off as i128).ok_or(ClockError::Overflow)
        }""")


def ncB5():
    # Inconsistent snapshot: the reader ignores a write in progress.
    patch(TK, """        if s0 & 1 == 1 {
            core::hint::spin_loop();
            continue;
        }""", """        let _ = s0;""")


def ncB6():
    # A torn user copy: the all-or-nothing copy writes before validating every byte.
    patch(UM, """        for i in 0..bytes.len() {
            let addr = (va.0 as usize)
                .checked_add(i)
                .ok_or(KernelError::UserMemoryFault)?;
            self.validate_user_access_for_asid(asid, addr, true)?;
        }
        self.copy_to_user(asid, va, bytes)""", """        for (i, b) in bytes.iter().enumerate() {
            let addr = (va.0 as usize)
                .checked_add(i)
                .ok_or(KernelError::UserMemoryFault)?;
            self.copy_to_user(asid, VirtAddr(addr as u64), core::slice::from_ref(b))?;
        }
        Ok(())""")


def ncB7():
    # Authorization dropped: any caller may set realtime (broad route).
    patch(CK, """    if kernel.task_class(tid) != Some(crate::kernel::task::TaskClass::SystemServer) {
        return Err(SyscallError::MissingRight);
    }""", """    let _ = kernel.task_class(tid);""")


# ── Checkpoint C: sleep ────────────────────────────────────────────────────────────────────
FX = "src/kernel/boot/futex.rs"
TASK = "src/kernel/task.rs"


def ncC1():
    # Early wake: a sleep is considered due a microsecond before its deadline.
    patch(FX, """    crate::kernel::timekeeping::monotonic_ns().is_ok_and(|now| now >= deadline_ns)""",
          """    crate::kernel::timekeeping::monotonic_ns().is_ok_and(|now| now + 1_000 >= deadline_ns)""")


def ncC2():
    # Wrong completion: a sleep's deadline is published as a timed-out FutexWait.
    patch(FX, """    let ending = if matches!(rec.key, FutexKey::Sleep { .. }) {
        FutexWaitEnding::SleepDone
    } else {""", """    let ending = if false {
        FutexWaitEnding::SleepDone
    } else {""")


def ncC3():
    # Runnable without a slot: a refused sleep placement makes the task Runnable anyway.
    patch(RT, """                let expired = !candidate.expired
                    && self.with_task_futex_split_mut(|tcbs, table| {
                        fx::expire_futex_waiter_locked(tcbs, table, candidate, now)
                    });""", """                let _ = self.with_task_futex_split_mut(|tcbs, table| {
                    fx::claim_futex_timeout_locked(tcbs, table, candidate, now)
                });
                let expired = !candidate.expired
                    && self.with_task_futex_split_mut(|tcbs, table| {
                        fx::expire_futex_waiter_locked(tcbs, table, candidate, now)
                    });""")


def ncC4():
    # Exit leaves the sleep record behind (no retirement at task death).
    patch("src/kernel/boot/restart_state.rs", """        // FUTEX-FOUND1: a dead task waits on nothing; retire any futex record naming it.
        self.with_task_futex_mut(|_tcbs, table| {
            super::futex::retire_futex_waiters_for_task_locked(table, tid)
        });""", """        // (control) no retirement""")


def ncC5():
    # A stale generation is accepted: the timeout claim compares only the slot.
    patch(FX, """    let slot = usize::from(candidate.token.slot);
    if slot >= MAX_FUTEX_WAITERS || table.token(slot) != candidate.token {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    rec.tid == candidate.tid""", """    let slot = usize::from(candidate.token.slot);
    if slot >= MAX_FUTEX_WAITERS {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    rec.tid == candidate.tid""")
    patch(FX, """    let slot = usize::from(candidate.token.slot);
    if table.token(slot) != candidate.token {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    let token = candidate.token;""", """    let slot = usize::from(candidate.token.slot);
    let Some(rec) = table.records[slot] else {
        return false;
    };
    let token = table.token(slot);""")


def ncC6():
    # Double completion: a sleep's TCB is retargeted to Runnable but its record survives, so the
    # next pass may complete it again.
    patch(FX, """    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    let ending = if""", """    tcb.status = TaskStatus::Runnable;
    if !matches!(rec.key, FutexKey::Sleep { .. }) {
        table.retire(slot);
    } else {
        tcb.status = TaskStatus::Blocked(WaitReason::Futex(token));
    }
    let ending = if""")


def ncC7():
    # Deadline overflow wraps into an early deadline.
    patch(TK, """    Ok(target.clamp(mono as i128, u64::MAX as i128) as u64)""",
          """    Ok(target.max(mono as i128) as u64)""")


def ncC8():
    # Realtime sleeps accepted before an epoch exists (an invented realtime of 0).
    patch(TK, """        ClockId::Realtime => Some(
            realtime_at(mono)?
                .to_signed_ns()
                .ok_or(ClockError::Overflow)?,""", """        ClockId::Realtime => Some(
            realtime_at(mono)
                .map_or(0, |t| t.to_signed_ns().unwrap_or(0)),""")


def ncC9():
    # The registration ignores the clock: a deadline already reached still parks.
    patch(FX, """    if sleep_deadline_reached(deadline_ns) {
        return Err(Refused::TimedOut);
    }
    let advances""", """    let advances""")


def ncC10():
    # The resume boundary forgets the Sleep class: its completion is never consumed.
    patch(IPC, """            .or_else(|| {
                self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::Sleep)
            })""", "")


# ── Live controls (the sched-time witness must FAIL its grader) ────────────────────────────
def ncL1():
    # Early wake by 50 ms: the 1 ms sleeps end before their bound.
    patch(FX, """    crate::kernel::timekeeping::monotonic_ns().is_ok_and(|now| now >= deadline_ns)""",
          """    crate::kernel::timekeeping::monotonic_ns().is_ok_and(|now| now + 50_000_000 >= deadline_ns)""")


def ncL2():
    # x86_64: a Sleep completion reaches the frame fields but not the restored registers.
    patch("src/arch/x86_64/trap.rs", """        crate::kernel::task::BlockedSyscallClass::FutexWait
            | crate::kernel::task::BlockedSyscallClass::Sleep
    ) {
        frame.commit_x86_syscall_returns();
    }""", """        crate::kernel::task::BlockedSyscallClass::FutexWait
    ) {
        frame.commit_x86_syscall_returns();
    }""")


CONTROLS = {k: v for k, v in globals().items() if k.startswith("nc") and callable(v)}

if __name__ == "__main__":
    CONTROLS[which]()
    print("applied", which)
