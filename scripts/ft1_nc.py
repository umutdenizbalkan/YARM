#!/usr/bin/env python3
"""FUTEX-TIME1 negative controls. Usage: ft1_nc.py <worktree> <ncN>
Each control reintroduces ONE defect into the candidate; the permanent hosted tests must catch it by
behaviour (a compile failure does not count)."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
FX = "src/kernel/boot/futex.rs"; RT = "src/runtime.rs"; SS = "src/kernel/syscall_split.rs"
SC = "src/kernel/syscall/sched.rs"; IP = "src/kernel/boot/ipc_state.rs"


def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))


def nc1():
    # A wake that wins a timed cycle publishes TimedOut (the wrong owner's result).
    patch(FX, """        publish_futex_wait_completion_locked(tcb, FutexWaitEnding::Woken);
        #[cfg(feature = "futex-time-witness")]""", """        publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);
        #[cfg(feature = "futex-time-witness")]""")


def nc2():
    # The timeout claim publishes Woken: "returned 1 because it merely stopped blocking".
    patch(FX, """    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);
    true
}""", """    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::Woken);
    true
}""")


def nc3():
    # The timeout claim leaves the record registered: a later wake can take the cycle again.
    patch(FX, """    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);
    true
}""", """    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);
    true
}""")


def nc4():
    # A wake of a timed cycle publishes no completion (the parked frame's provisional leaks out).
    patch(FX, """    if rec.deadline.is_some() {
        publish_futex_wait_completion_locked(tcb, FutexWaitEnding::Woken);""", """    if rec.deadline.is_some() && false {
        publish_futex_wait_completion_locked(tcb, FutexWaitEnding::Woken);""")


def nc5():
    # The compare-and-register ignores an already reached deadline: a zero timeout parks.
    patch(FX, """            if now() >= deadline {
                return Err(Refused::TimedOut);
            }""", """            if now() > deadline.saturating_add(u64::MAX / 2) {
                return Err(Refused::TimedOut);
            }""")


def nc6():
    # The timeout ignores an NR 17 Pending key: it races the requeue's committed wake.
    patch(FX, """        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot)
        {""", """        if deadline > now || !table.live(tcbs, slot) {""")
    patch(FX, """        && rec.deadline.is_some_and(|d| d <= now)
        && !matches!(rec.key, FutexKey::Pending { .. })
        && table.live(tcbs, slot)""", """        && rec.deadline.is_some_and(|d| d <= now)
        && table.live(tcbs, slot)""")


def nc7():
    # The collector times out waits that are not due yet.
    patch(FX, """        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot)
        {""", """        if matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot) {""")
    patch(FX, """        && rec.deadline.is_some_and(|d| d <= now)
        && !matches!""", """        && rec.deadline.is_some_and(|d| d <= now.saturating_add(u64::MAX / 2))
        && !matches!""")


def nc8():
    # Capacity refused: the collector makes the waiter Runnable anyway (Runnable but unqueued).
    patch(RT, """                let expired = !candidate.expired
                    && self.with_task_futex_split_mut(|tcbs, table| {
                        fx::expire_futex_waiter_locked(tcbs, table, candidate, now)
                    });""", """                let expired = !candidate.expired
                    && self.with_task_futex_split_mut(|tcbs, table| {
                        fx::claim_futex_timeout_locked(tcbs, table, candidate, now)
                    });""")


def nc9():
    # The tick conversion drops the partial admission period: a wait can end before `ns`.
    patch(SC, "        timeout_ns.div_ceil(period_ns) + 1\n", "        timeout_ns.div_ceil(period_ns)\n")


def nc10():
    # A retired timed record is not uncounted: the collector's timed census leaks.
    patch(FX, """        if rec.deadline.is_some() {
            self.timed = self.timed.saturating_sub(1);""", """        if rec.deadline.is_some() && rec.tid == u64::MAX {
            self.timed = self.timed.saturating_sub(1);""")


def nc11():
    # The arm does not advance the wait generation: a stale cycle's completion matches.
    patch(FX, "    tcb.futex_wait_generation = next;\n    rec.deadline = Some(deadline);",
          "    let _ = next;\n    rec.deadline = Some(deadline);")


def nc12():
    # The split route precommits 1 into a TIMED wait's parked frame (both parked sites).
    patch(SS, """            if deadline.is_some() {
                frame.set_err(SyscallError::Internal.code());
            } else {
                frame.set_ok(1, 0, 0);
            }""", """            if deadline.is_some() {
                frame.set_ok(1, 0, 0);
            } else {
                frame.set_ok(1, 0, 0);
            }""", count=2)


def nc13():
    # The resume boundary never consumes a FutexWait completion.
    patch(IP, """        self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::IpcSend)
            .or_else(|| {
                self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::FutexWait)
            })""", """        self.take_blocked_syscall_completion_of_class(tid, BlockedSyscallClass::IpcSend)""")


def nc14():
    # The collector's cursor never advances: refused slots starve the rest of the table.
    patch(FX, "    table.timeout_cursor = ((start + examined) % cap.max(1)) as u16;\n",
          "    let _ = examined;\n")


def nc15():
    # The timeout is read even without FUTEX_FLAG_TIMED: untimed waits change meaning.
    patch(SC, "    let timeout_ns = (flags & FUTEX_FLAG_TIMED != 0).then(|| frame.arg(3) as u64);",
          "    let timeout_ns = (frame.arg(3) != 0 || flags & FUTEX_FLAG_TIMED != 0).then(|| frame.arg(3) as u64);")


def nc16():
    # The deadline wraps instead of refusing: an overflowing timeout expires at once.
    patch(SC, "    let deadline = now.checked_add(ticks);\n", "    let deadline = Some(now.wrapping_add(ticks));\n")


def nc17():
    # The timeout claim no longer insists on the exact Blocked(Futex(token)) cycle.
    patch(FX, """        .filter(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })
    else {
        return false;
    };
    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);""", """        .filter(|t| {
            let _ = token;
            t.tid.0 == rec.tid
        })
    else {
        return false;
    };
    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);""")


def nc18():
    # Expiry leaves the record on its user word: a later wake still "wins" a decided cycle.
    patch(FX, """    rekey(
        table,
        slot,
        prev,
        FutexKey::Expired { tid: candidate.tid },
        None,
    );
    true
}""", """    let _ = prev;
    true
}""")


def nc19():
    # The scan window is unbounded (examines every timed record each pass).
    patch(FX, "    while examined < cap && timed_seen < FUTEX_TIMEOUT_SCAN_WINDOW && n < FUTEX_TIMEOUT_DUE_MAX {",
          "    while examined < cap && n < FUTEX_TIMEOUT_DUE_MAX {")


def nc20():
    # Brief: "Attach the deadline after releasing the registration lock." The hold registers the
    # record untimed; the park path attaches the deadline in a SEPARATE task-lock hold afterwards.
    patch(FX, """    if let Some(deadline) = deadline
        && !arm_futex_deadline_locked(tcbs, table, token, deadline)
    {""", """    if let Some(deadline) = deadline
        && deadline == u64::MAX
        && !arm_futex_deadline_locked(tcbs, table, token, deadline)
    {""")
    patch(FX, "fn arm_futex_deadline_locked(\n", "pub(crate) fn arm_futex_deadline_locked(\n")
    patch(RT, """        #[cfg(test)]
        fx::futex_phase_hook::run_after_register();
""", """        #[cfg(test)]
        fx::futex_phase_hook::run_after_register();
        if let Some(d) = deadline {
            let _ = self.with_task_futex_split_mut(|tcbs, table| {
                fx::arm_futex_deadline_locked(tcbs, table, token, d)
            });
        }
""")


def nc21():
    # Brief: "Lose a timeout when its home queue is full." A refused reservation drops the record
    # (and its deadline): the waiter stays blocked with nothing left that can end its wait.
    patch(RT, """                        fx::expire_futex_waiter_locked(tcbs, table, candidate, now)
                    });""", """                        let _ = (tcbs, now);
                        fx::retire_futex_waiters_for_task_locked(table, candidate.tid) > 0
                    });""")


def nc22():
    # Brief: "Cancel by the original key after requeue." A requeue leaves a TIMED waiter on its
    # source key (counted as moved), so the source key can still end it.
    patch(FX, "            rekey(table, slot, prev, t_key, Some(target.0 as u64));\n",
          """            if table.records[slot].is_some_and(|r| r.deadline.is_some()) {
                let _ = prev;
            } else {
                rekey(table, slot, prev, t_key, Some(target.0 as u64));
            }
""")


def nc23():
    # Brief: "Resurrect an exiting task." Exit keeps a dying task's TIMED record, and the timeout
    # claim no longer insists on a live Blocked(Futex(token)) cycle.
    patch(FX, "        if table.records[slot].is_some_and(|r| r.tid == tid) {\n            table.retire(slot);",
          "        if table.records[slot].is_some_and(|r| r.tid == tid && r.deadline.is_none()) {\n            table.retire(slot);")
    patch(FX, """        && !matches!(rec.key, FutexKey::Pending { .. })
        && table.live(tcbs, slot)
}""", """        && !matches!(rec.key, FutexKey::Pending { .. })
        && !tcbs.is_empty()
}""")
    patch(FX, """        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot)
        {""", """        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) {""")
    patch(FX, """        .filter(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })
    else {
        return false;
    };
    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);""", """        .filter(|t| {
            let _ = token;
            t.tid.0 == rec.tid
        })
    else {
        return false;
    };
    tcb.status = TaskStatus::Runnable;
    table.retire(slot);
    publish_futex_wait_completion_locked(tcb, FutexWaitEnding::TimedOut);""")


def nc24():
    # Brief: "Skip an unplaceable due waiter forever." The collector never hands out a waiter
    # whose timeout already won but whose placement is still owed.
    patch(FX, """        if deadline > now || matches!(rec.key, FutexKey::Pending { .. }) || !table.live(tcbs, slot)
        {""", """        if deadline > now
            || matches!(rec.key, FutexKey::Pending { .. } | FutexKey::Expired { .. })
            || !table.live(tcbs, slot)
        {""")


def nc25():
    # Brief: "Restore the wrong parked result on RISC-V." The result register the RISC-V
    # write-back installs for a timed wait reads the success value whatever ended it.
    patch("src/kernel/task.rs", """        if self.result == 0 {
            self.success_ret0()
        } else {
            self.result as usize
        }""", """        if self.result == 0 || matches!(self.syscall_class, BlockedSyscallClass::FutexWait) {
            self.success_ret0()
        } else {
            self.result as usize
        }""")


def nc26():
    # Brief: "Lose the correct timeout result during context restore." The thread-restore facts
    # carry no FutexWait completion.
    patch("src/kernel/task.rs", """    let send_completion = send_completion.or_else(|| {
        take_blocked_syscall_completion_for(tcbs, tid, expect_asid, BlockedSyscallClass::FutexWait)
    });""", """    let send_completion = send_completion.or(None);""")


def nc27():
    # Brief: "Stop clock progress when the system becomes idle." The clock owner advances the
    # scheduler tick only while it has a current task.
    patch("src/kernel/scheduler.rs", """        let now = if self.idx == CLOCK_OWNER_CPU.0 as usize {
            self.clock.fetch_add(1, Ordering::AcqRel).wrapping_add(1)""", """        let now = if self.idx == CLOCK_OWNER_CPU.0 as usize && self.current_tid().is_some() {
            self.clock.fetch_add(1, Ordering::AcqRel).wrapping_add(1)""")


def nc28():
    # The live x86_64 defect FUTEX-TIME1 found and repaired: the completion is encoded into the
    # frame's fields only, never into the continuation's return-lane registers.
    patch("src/arch/x86_64/trap.rs", """    if done.syscall_class == crate::kernel::task::BlockedSyscallClass::FutexWait {
        frame.commit_x86_syscall_returns();
    }""", """    let _ = done.syscall_class;""")


def nc29():
    # Brief: "Leave a deadline attached to a recycled slot." A retired timed record's deadline is
    # kept with its SLOT, and the next registration in that slot (any task, timed or not) takes it.
    patch(FX, """        self.records[slot] = None;
        self.generations[slot] = self.generations[slot].wrapping_add(1);""", """        if let Some(d) = rec.deadline {
            NC29_SLOT_DEADLINE[slot].store(d.saturating_add(1), core::sync::atomic::Ordering::SeqCst);
        }
        self.records[slot] = None;
        self.generations[slot] = self.generations[slot].wrapping_add(1);""")
    patch(FX, "pub fn register_futex_waiter_locked(\n", """static NC29_SLOT_DEADLINE: [core::sync::atomic::AtomicU64; MAX_FUTEX_WAITERS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; MAX_FUTEX_WAITERS];

pub fn register_futex_waiter_locked(
""")
    patch(FX, """        va,
        deadline: None,
        #[cfg(feature = "futex-time-witness")]
        g_race: false,
        next: NIL,
    });
    table.link_tail(slot);""", """        va,
        deadline: match NC29_SLOT_DEADLINE[slot].swap(0, core::sync::atomic::Ordering::SeqCst) {
            0 => None,
            d => Some(d - 1),
        },
        #[cfg(feature = "futex-time-witness")]
        g_race: false,
        next: NIL,
    });
    if table.records[slot].is_some_and(|r| r.deadline.is_some()) {
        table.timed = table.timed.saturating_add(1);
        FUTEX_TIMED_RECORDS.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    }
    table.link_tail(slot);""")


globals()[which]()
print(f"applied {which}")
