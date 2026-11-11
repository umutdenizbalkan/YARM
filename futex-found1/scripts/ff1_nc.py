#!/usr/bin/env python3
"""FUTEX-FOUND1 negative controls. Usage: ff1_nc.py <worktree> <ncN>
Each control reintroduces ONE defect class into the candidate."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
F = "src/kernel/boot/futex.rs"
X = "src/kernel/boot/exec_state.rs"
R = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))
PRIV = '''        FutexKind::Private => FutexKey::Private {
            asid,
            aspace_generation,
            va: addr as u64,
        },'''
CLAIM = '''            match tcbs
                .get_mut(usize::from(rec.tcb))
                .and_then(Option::as_mut)
                .filter(|t| {
                    t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
                }) {
                Some(tcb) => {
                    tcb.status = TaskStatus::Runnable;
                    out[n] = rec.tid;
                    n += 1;
                    true
                }'''
C = {
 # NC1 numeric-VA-only matching: the private key forgets which address space it is in
 "nc1": [(F, PRIV, '''        FutexKind::Private => FutexKey::Private {
            asid: Asid(0),
            aspace_generation: 0,
            va: addr as u64,
        },''')],
 # NC2 missing address-space generation: a reused ASID number is the same key
 "nc2": [(F, PRIV, '''        FutexKind::Private => FutexKey::Private {
            asid,
            aspace_generation: { let _ = aspace_generation; 0 },
            va: addr as u64,
        },''')],
 # NC3 missing shared-backing offset: every word of an object is one key
 "nc3": [(F, '''                offset: phys - object.phys.0,''', '''                offset: { let _ = object.phys; 0 },''')],
 # NC4 non-atomic compare/publication: no re-read once the waiter is visible
 "nc4": [(R, '''        let unchanged = matches!(reread, Ok((k, w)) if k == key && w == expected);''',
          '''        let unchanged = { let _ = &reread; true };''')],
 # NC5 duplicate waiter claim: the claim neither checks the waiter's state nor retires its record
 "nc5": [(F, CLAIM, '''            match tcbs
                .get_mut(usize::from(rec.tcb))
                .and_then(Option::as_mut)
                .filter(|t| { let _ = token; t.tid.0 == rec.tid }) {
                Some(tcb) => {
                    tcb.status = TaskStatus::Runnable;
                    out[n] = rec.tid;
                    n += 1;
                    false
                }''')],
 # NC6 stale wait-generation acceptance: undo and read-back accept ANY futex wait
 "nc6": [(F, '''        .find(|t| t.tid.0 == tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token)))
    else {''', '''        .find(|t| { let _ = token; t.tid.0 == tid && matches!(t.status, TaskStatus::Blocked(WaitReason::Futex(_))) })
    else {'''),
          (F, '''        .any(|t| t.tid.0 == tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token)))''',
           '''        .any(|t| { let _ = token; t.tid.0 == tid && matches!(t.status, TaskStatus::Blocked(WaitReason::Futex(_))) })''')],
 # NC7 incorrect FIFO removal: unlinking forgets the predecessor's link
 "nc7": [(F, '''        } else if let Some(p) = self.records[prev as usize].as_mut() {
            p.next = rec.next;
        }''', '''        }''')],
 # NC8 capacity refusal without task rollback: the status is written before the refusal
 "nc8": [(F, '''    let Some(slot) = free else {
        return Err(FutexRegisterRefusal::Full);
    };''', '''    let Some(slot) = free else {
        if let Some(tcb) = tcbs[tcb_idx].as_mut() {
            tcb.status = TaskStatus::Blocked(WaitReason::Futex(table.token(0)));
        }
        return Err(FutexRegisterRefusal::Full);
    };''')],
 # NC9 wake without verified Runnable transition: claimed and counted, never made Runnable
 "nc9": [(F, CLAIM, CLAIM.replace("                    tcb.status = TaskStatus::Runnable;\n", "                    let _ = &tcb;\n"))],
 # NC10 cleanup against the wrong task: robust cleanup in the DRIVING task's address space
 "nc10": [(X, '''        let asid = self
            .task_asid(exiting_tid)
            .ok_or(KernelError::UserMemoryFault)?;''', '''        let _ = exiting_tid;
        let asid = self
            .current_tid()
            .and_then(|t| self.task_asid(t))
            .ok_or(KernelError::UserMemoryFault)?;''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
