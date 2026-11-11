#!/usr/bin/env python3
"""FUTEX-REQUEUE1 negative controls. Usage: frq_nc.py <worktree> <ncN>
Each control reintroduces ONE defect class into the candidate; the permanent tests must catch it."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
F = "src/kernel/boot/futex.rs"; R = "src/runtime.rs"; X = "src/kernel/boot/exec_state.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))
NO_PLACE = '''                    "FUTEX_WAKE_STOPPED tid={} cpu={} reason=no_placement err={:?} woke={}",
                    candidate.tid,
                    target.0,
                    err,
                    woke
                );
'''
C = {
 # NC1 skip the comparison (re)check: a stale `expected` still moves and wakes.
 "nc1": [(F, '''    if word != expected {
        return Err(FutexRequeueRefusal::ValueChanged);
    }
    let b = s_key.bucket();''', '''    let _ = (word, expected);
    let b = s_key.bucket();''')],
 # NC2 VA-only identity: every bucket walked, records matched by their VA alone.
 "nc2": [(F, '''    let b = s_key.bucket();
    let last = table.tail[b];
    let mut prev = NIL;
    let mut cur = table.head[b];
    let (mut kept, mut moved) = (0u32, 0u32);
    while cur != NIL && moved < max_move {''', '''    let (mut kept, mut moved) = (0u32, 0u32);
    for b in 0..FUTEX_BUCKETS {
    let last = table.tail[b];
    let mut prev = NIL;
    let mut cur = table.head[b];
    while cur != NIL && moved < max_move {'''),
         (F, '''        } else if rec.key != s_key {
            prev = cur;''', '''        } else if rec.va != source.0 as u64 {
            prev = cur;'''),
         (F, '''        cur = next;
    }
    Ok(FutexRequeueTransfer {''', '''        cur = next;
    }
    }
    Ok(FutexRequeueTransfer {''')],
 # NC3 move a stale token: liveness is not checked, so dead cycles are moved and counted.
 "nc3": [(F, '''        if !table.live(tcbs, slot) {
            // Stale: reclaimed on the way, exactly as a wake does.''', '''        if false && !table.live(tcbs, slot) {
            // Stale: reclaimed on the way, exactly as a wake does.''')],
 # NC4 invalidate the token during the move (a new incarnation for the moved record).
 "nc4": [(F, '''            table.link_tail(slot);
            moved += 1;''', '''            table.link_tail(slot);
            table.generations[slot] = table.generations[slot].wrapping_add(1);
            moved += 1;''')],
 # NC5 corrupt FIFO: moved records are pushed at the HEAD of the target chain.
 "nc5": [(F, '''            table.link_tail(slot);
            moved += 1;''', '''            let tb = t_key.bucket();
            if let Some(r) = table.records[slot].as_mut() {
                r.next = table.head[tb];
            }
            if table.head[tb] == NIL {
                table.tail[tb] = slot as u16;
            }
            table.head[tb] = slot as u16;
            moved += 1;''')],
 # NC6 same-bucket shortcut: when both keys share a bucket, retag in place (no relink).
 "nc6": [(F, '''        } else {
            table.unlink_after(slot, prev);''', '''        } else if t_key.bucket() == b {
            if let Some(r) = table.records[slot].as_mut() {
                r.key = t_key;
                r.va = target.0 as u64;
            }
            moved += 1;
            prev = cur;
        } else {
            table.unlink_after(slot, prev);''')],
 # NC7 a requeued waiter is made Runnable (and placed nowhere).
 "nc7": [(F, '''pub fn compare_and_requeue_locked(
    tcbs: &[Option<ThreadControlBlock>],''', '''pub fn compare_and_requeue_locked(
    tcbs: &mut [Option<ThreadControlBlock>],'''),
         (F, '''            table.link_tail(slot);
            moved += 1;''', '''            table.link_tail(slot);
            if let Some(t) = tcbs.get_mut(usize::from(rec.tcb)).and_then(Option::as_mut) {
                t.status = TaskStatus::Runnable;
            }
            moved += 1;''')],
 # NC8 a waiter whose reservation is refused is claimed anyway (discarded: Runnable, unplaced).
 "nc8": [(R, NO_PLACE, NO_PLACE + '''                let _ = self.with_task_futex_split_mut(|tcbs, table| {
                    fx::claim_futex_waiter_locked(tcbs, table, key, candidate)
                });
'''),
         (X, NO_PLACE, NO_PLACE + '''                let _ = self.with_task_futex_mut(|tcbs, table| {
                    fx::claim_futex_waiter_locked(tcbs.as_mut_slice(), table, key, candidate)
                });
''')],
 # NC9 duplicate ownership: the claim does not check that the record is still the woken key's.
 "nc9": [(F, '''    if rec.key != key || rec.tid != candidate.tid {''', '''    if rec.tid != candidate.tid {''')],
 # NC10 ignore the split-wait publication race: the undo assumes the waiter is on its own key.
 "nc10": [(R, '''        let undo_registration = || {
            self.with_task_futex_split_mut(|tcbs, table| {
                fx::undo_futex_registration_locked(tcbs, table, tid, token)
            })
        };''', '''        let undo_registration = || {
            self.with_task_futex_split_mut(|tcbs, table| {
                if table.key_of(token) != Some(key) {
                    return false;
                }
                fx::undo_futex_registration_locked(tcbs, table, tid, token)
            })
        };''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
