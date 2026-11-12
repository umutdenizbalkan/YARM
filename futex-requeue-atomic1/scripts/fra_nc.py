#!/usr/bin/env python3
"""FUTEX-REQUEUE-ATOMIC1 negative controls. Usage: fra_nc.py <worktree> <ncN>
Each control reintroduces ONE defect class into the candidate; the permanent tests must catch it."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
F = "src/kernel/boot/futex.rs"; R = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))

SEL = '''        } else if selected < woken
            && let Some((cpu, priority)) = record_placement(tcbs, classes, table, slot)
        {'''
PEND_KEY = '''            let key = FutexKey::Pending {
                op: committed.op,
                cpu: home.0,
                priority: priority as u8,
            };
            for _ in 0..count {
                let Some(tid) = self.with_task_futex_split_mut(|tcbs, table| {
                    let tid = fx::claim_pending_futex_waiter_locked(tcbs, table, key)?;'''
C = {
 # NC1 the gap restored: the wake portion stays ON THE SOURCE at the commit, and the placement
 # wakes whoever is oldest on the source when it runs.
 "nc1": [(F, '''            rekey(table, slot, prev, pending, None);
            assigned.add(cpu, priority);
            selected += 1;''', '''            let _ = pending;
            prev = cur;
            assigned.add(cpu, priority);
            selected += 1;'''),
         (R, PEND_KEY, PEND_KEY.replace('''            let key = FutexKey::Pending {
                op: committed.op,
                cpu: home.0,
                priority: priority as u8,
            };''', '''            let _ = FutexKey::Pending {
                op: committed.op,
                cpu: home.0,
                priority: priority as u8,
            };
            let key = committed.source;'''))],
 # NC2 a newly registered source waiter can be claimed by the delayed placement: when the
 # pending key has no waiter left, the placement falls back to the source.
 "nc2": [(R, PEND_KEY, PEND_KEY.replace('''                    let tid = fx::claim_pending_futex_waiter_locked(tcbs, table, key)?;''', '''                    let tid = fx::claim_pending_futex_waiter_locked(tcbs, table, key).or_else(|| {
                        fx::claim_pending_futex_waiter_locked(tcbs, table, committed.source)
                    })?;'''))],
 # NC3 the claim checks the TID only, not the exact token or key.
 "nc3": [(F, '''    if slot >= MAX_FUTEX_WAITERS || table.token(slot) != candidate.token {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    if rec.key != key || rec.tid != candidate.tid {''', '''    if slot >= MAX_FUTEX_WAITERS {
        return false;
    }
    let Some(rec) = table.records[slot] else {
        return false;
    };
    let _ = key;
    if rec.tid != candidate.tid {'''),
         (F, '''    let token = candidate.token;
    let Some(tcb) = tcbs
        .get_mut(usize::from(rec.tcb))
        .and_then(Option::as_mut)
        .filter(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })''', '''    let Some(tcb) = tcbs
        .get_mut(usize::from(rec.tcb))
        .and_then(Option::as_mut)
        .filter(|t| t.tid.0 == rec.tid && matches!(t.status, TaskStatus::Blocked(_)))''')],
 # NC4 the commit moves stale records (liveness unchecked in the moving walk).
 "nc4": [(F, '''        let at_last = cur == last;
        if !table.live(tcbs, slot) {
            table.retire_after(slot, prev);
        } else if rec.key != s_key {
            prev = cur;
        } else if selected < woken''', '''        let at_last = cur == last;
        if false && !table.live(tcbs, slot) {
            table.retire_after(slot, prev);
        } else if rec.key != s_key {
            prev = cur;
        } else if selected < woken''')],
 # NC5 the reservations are released right after the commit, before the claims, and the
 # placement uses an ordinary enqueue.
 "nc5": [(R, '''            #[cfg(test)]
            fx::futex_phase_hook::run_requeue_window();
            #[cfg(feature = "futex-requeue-race-witness")]''', '''            let mut early = preflight.held;
            self.futex_release_placements_split(&mut early);
            #[cfg(test)]
            fx::futex_phase_hook::run_requeue_window();
            #[cfg(feature = "futex-requeue-race-witness")]'''),
         (R, '''            self.futex_requeue_place_split(&committed, &mut preflight.held);
            self.futex_release_placements_split(&mut preflight.held);
            return Ok(fx::FutexRequeueOutcome {''', '''            self.futex_requeue_place_split(&committed, &mut preflight.held);
            preflight.held = fx::FutexPlacementCounts::new();
            return Ok(fx::FutexRequeueOutcome {'''),
         (R, '''                match self.with_cpu_sched_split(home, |c| {
                    te::commit_reserved_enqueue_on_cpu_locked(c, &plan)
                }) {''', '''                match self.with_cpu_sched_split(home, |c| {
                    te::commit_enqueue_on_cpu_locked(c, &plan).map(ReservedCommit::Queued)
                }) {''')],
 # NC6 an abandoned reservation (its waiter left the pending key first) is never released.
 "nc6": [(R, '''            self.futex_requeue_place_split(&committed, &mut preflight.held);
            self.futex_release_placements_split(&mut preflight.held);''', '''            self.futex_requeue_place_split(&committed, &mut preflight.held);''')],
 # NC7 the commit selects the wake portion without secured capacity, and the placement enqueues
 # without a reservation.
 "nc7": [(F, '''                Some((cpu, priority)) if avail.take(cpu, priority) => woken += 1,''', '''                Some((cpu, priority)) if {
                    let _ = avail.take(cpu, priority);
                    true
                } =>
                {
                    woken += 1
                }'''),
         (R, '''                match self.with_cpu_sched_split(home, |c| {
                    te::commit_reserved_enqueue_on_cpu_locked(c, &plan)
                }) {''', '''                match self.with_cpu_sched_split(home, |c| {
                    te::commit_enqueue_on_cpu_locked(c, &plan).map(ReservedCommit::Queued)
                }) {''')],
 # NC8 the wake portion walks past an unplaceable head and takes later waiters with room
 # (the preflight keeps reserving past a refusal; the commit skips a waiter it cannot place).
 "nc8": [("src/kernel/boot/exec_state.rs", '''        let mut cursor = None;
        for _ in 0..want {
            let Ok(Some(candidate)) = self.with_task_futex_mut(|tcbs, table| {''', '''        let mut cursor = None;
        for _ in 0..fx::MAX_FUTEX_WAITERS {
            if out.held.total() as usize >= want {
                break;
            }
            let Ok(Some(candidate)) = self.with_task_futex_mut(|tcbs, table| {'''),
         ("src/kernel/boot/exec_state.rs", '''                out.refused = Some(FutexPlacementRefusal::Slot {
                    cpu: home,
                    priority: plan.priority,
                });
                break;''', '''                out.refused = Some(FutexPlacementRefusal::Slot {
                    cpu: home,
                    priority: plan.priority,
                });
                cursor = Some(candidate);
                continue;'''),
         (R, '''        let mut cursor = None;
        for _ in 0..want {
            // rank 2: the next source waiter in FIFO order, then its plan.''', '''        let mut cursor = None;
        for _ in 0..fx::MAX_FUTEX_WAITERS {
            if out.held.total() as usize >= want {
                break;
            }
            // rank 2: the next source waiter in FIFO order, then its plan.'''),
         (R, '''                out.refused = Some(FutexPlacementRefusal::Slot {
                    cpu: home,
                    priority: plan.priority,
                });
                break;''', '''                out.refused = Some(FutexPlacementRefusal::Slot {
                    cpu: home,
                    priority: plan.priority,
                });
                cursor = Some(candidate);
                continue;'''),
         (F, '''                    if !explained && !preflight.last_attempt {
                        return Ok(FutexRequeueCommit::Stale);
                    }
                    capacity_limited = true;
                    break;''', '''                    let _ = explained;
                    capacity_limited = true;'''),
         (F, SEL, '''        } else if selected < woken
            && !record_placement(tcbs, classes, table, slot)
                .is_some_and(|(c, p)| assigned_room(&preflight.held, &assigned, c, p))
        {
            prev = cur;
        } else if selected < woken
            && let Some((cpu, priority)) = record_placement(tcbs, classes, table, slot)
        {'''),
         (F, '''/// rank 2 — claim the oldest waiter still on the pending key''', '''fn assigned_room(
    held: &FutexPlacementCounts,
    assigned: &FutexPlacementCounts,
    cpu: CpuId,
    priority: TaskPriority,
) -> bool {
    let mut h = *held;
    for (c, p, n) in assigned.nonzero() {
        for _ in 0..n {
            h.take(c, p);
        }
    }
    h.take(cpu, priority)
}

/// rank 2 — claim the oldest waiter still on the pending key''')],
 # NC9 source FIFO corrupted: the oldest live source waiters go to the target first, so the wake
 # portion is not the oldest waiters.
 "nc9": [(F, SEL, '''        } else if woken > 0 && selected == 0 && moved < max_move.min(2) {
            rekey(table, slot, prev, t_key, Some(target.0 as u64));
            moved += 1;
        } else if selected < woken
            && let Some((cpu, priority)) = record_placement(tcbs, classes, table, slot)
        {''')],
 # NC10 target FIFO corrupted: moved records are linked at the HEAD of the target's bucket.
 "nc10": [(F, '''            rekey(table, slot, prev, t_key, Some(target.0 as u64));
            moved += 1;
        } else {''', '''            table.unlink_after(slot, prev);
            let tb = t_key.bucket();
            if let Some(r) = table.records[slot].as_mut() {
                r.key = t_key;
                r.va = target.0 as u64;
                r.next = table.head[tb];
            }
            if table.head[tb] == NIL {
                table.tail[tb] = slot as u16;
            }
            table.head[tb] = slot as u16;
            moved += 1;
        } else {''')],
 # NC11 one record on two queues: the move links the record onto the target without unlinking
 # it from the source, so the source walk and a competing requeue can still reach it there.
 "nc11": [(F, '''fn rekey(table: &mut FutexWaiterTable, slot: usize, prev: u16, key: FutexKey, va: Option<u64>) {
    table.unlink_after(slot, prev);''', '''fn rekey(table: &mut FutexWaiterTable, slot: usize, prev: u16, key: FutexKey, va: Option<u64>) {
    let _ = prev;''')],
 # NC12 the split park's read-back recovery restores the wrong incarnation (another TID).
 "nc12": [(R, '''        let entering = crate::kernel::recv_waiter_split::RecvEnteringIncarnation {
            tid,
            asid,
            priority,
        };
        let recovered = self.restore_entering_incarnation_exact_split(cpu, entering);''', '''        let entering = crate::kernel::recv_waiter_split::RecvEnteringIncarnation {
            tid: tid + 1,
            asid,
            priority,
        };
        let recovered = self.restore_entering_incarnation_exact_split(cpu, entering);''')],
 # NC13 the split wait's second-read undo retracts a registration another operation acted on
 # (the FUTEX-REQUEUE1 rule): a moved or selected waiter returns ValueChanged.
 "nc13": [(R, '''                fx::undo_futex_registration_unless_acted_on_locked(tcbs, table, tid, token, key)''', '''                fx::undo_futex_registration_locked(tcbs, table, tid, token)''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
