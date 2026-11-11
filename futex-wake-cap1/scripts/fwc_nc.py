#!/usr/bin/env python3
"""FUTEX-WAKE-CAP1 negative controls. Usage: fwc_nc.py <worktree> <ncN>
Each control reintroduces ONE defect class into the candidate."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
SC = "src/kernel/scheduler.rs"; TE = "src/kernel/task_enqueue.rs"; F = "src/kernel/boot/futex.rs"
R = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))
C = {
 # NC1 claim before the placement is secured (the baseline order): the reservation is a no-op
 # and the commit is the ordinary, refusable enqueue.
 "nc1": [(TE, '''        EnqueueTarget::Pinned(target) if target == view.cpu() => view
            .reserve_user_slot(plan.priority)
            .map_err(map_scheduler_error),''', '''        EnqueueTarget::Pinned(target) if target == view.cpu() => Ok(()),'''),
         (TE, '''    match view.enqueue_reserved_with_priority(ThreadId(plan.tid), plan.priority) {''',
              '''    match view.enqueue_with_priority(ThreadId(plan.tid), plan.priority) {''')],
 # NC2 the reservation ignores the capacity bound
 "nc2": [(SC, '''    pub(crate) fn reserve_user_slot(
        &mut self,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        if !self.user_room(priority) {
            return Err(SchedulerError::QueueFull);
        }
        self.reserved''', '''    pub(crate) fn reserve_user_slot(
        &mut self,
        priority: TaskPriority,
    ) -> Result<(), SchedulerError> {
        self.reserved''')],
 # NC3 a failed claim keeps its reservation
 "nc3": [(R, '''            if !claimed {
                let _ = self.with_cpu_sched_split(target, |c| {
                    te::release_enqueue_reservation_on_cpu_locked(c, &plan)
                });
                continue;
            }''', '''            if !claimed {
                continue;
            }''')],
 # NC4 a reserved fill does not consume its reservation
 "nc4": [(SC, '''        self.release_user_slot(priority);
        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }''', '''        if self.contains_tid(tid) {
            return Err(SchedulerError::AlreadyQueued);
        }''')],
 # NC5 ordinary user pushes ignore reservations
 "nc5": [(SC, '''        if !crate::kernel::kthread::is_kthread_tid(tid.0) && !self.user_room(priority) {
            return Err(SchedulerError::QueueFull);
        }
        self.push_unchecked_capacity(tid, priority)''', '''        self.queues[Self::priority_index(priority)].push_within(
            tid,
            if crate::kernel::kthread::is_kthread_tid(tid.0) { MAX_QUEUED_PER_PRIORITY } else { MAX_RUN_QUEUE },
        )?;
        if !self.membership_tracking_exhausted && self.membership_insert(tid).is_err() {
            self.membership_tracking_exhausted = self.rebuild_membership_table();
        }
        Ok(())''')],
 # NC6 a waiter already placed (mid-publication) is not counted
 "nc6": [(R, '''                Ok(Ok(ReservedCommit::AlreadyPlaced(_))) => woke += 1,''',
              '''                Ok(Ok(ReservedCommit::AlreadyPlaced(_))) => {}''')],
 # NC7 the claim does not check the waiter's state (only its TID)
 "nc7": [(F, '''        .filter(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })
    else {
        return false;
    };''', '''        .filter(|t| {
            let _ = token;
            t.tid.0 == rec.tid
        })
    else {
        return false;
    };''')],
 # NC8 the wake takes the NEWEST live waiter, not the oldest
 "nc8": [(F, '''        if table.live(tcbs, slot) {
            if rec.key == key {
                return Some(FutexWakeCandidate {
                    tid: rec.tid,
                    token: table.token(slot),
                });
            }
            prev = cur;''', '''        if table.live(tcbs, slot) {
            if rec.key == key {
                newest = Some(FutexWakeCandidate {
                    tid: rec.tid,
                    token: table.token(slot),
                });
            }
            prev = cur;'''),
         (F, '''        cur = next;
    }
    None
}''', '''        cur = next;
    }
    newest
}'''),
         (F, '''    let b = key.bucket();
    let mut prev = NIL;
    let mut cur = table.head[b];
    while cur != NIL {
        let slot = usize::from(cur);
        let rec = table.records[slot]?;''', '''    let b = key.bucket();
    let mut newest = None;
    let mut prev = NIL;
    let mut cur = table.head[b];
    while cur != NIL {
        let slot = usize::from(cur);
        let rec = table.records[slot]?;''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
