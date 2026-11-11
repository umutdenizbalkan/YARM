#!/usr/bin/env python3
"""FUTEX-FOUND1 part F: O(1) record liveness (the TCB slot rides in the record), dead-code gating,
and the clippy class parity. Usage: ff1_patch_f.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))
F = "src/kernel/boot/futex.rs"
patch(F, '''struct FutexWaiterRecord {
    key: FutexKey,
    tid: u64,''', '''struct FutexWaiterRecord {
    key: FutexKey,
    tid: u64,
    /// The waiter's TCB slot, so liveness is one comparison and not a TCB-array scan. A TCB never
    /// moves slots; a slot reused by another task fails the `tid` and token comparison.
    tcb: u16,''')
patch(F, '''        let token = self.token(slot);
        tcbs.iter().flatten().any(|t| {
            t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
        })
    }''', '''        let token = self.token(slot);
        tcbs.get(usize::from(rec.tcb))
            .and_then(Option::as_ref)
            .is_some_and(|t| {
                t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
            })
    }''')
patch(F, '''    table.records[slot] = Some(FutexWaiterRecord {
        key,
        tid,
        va,''', '''    table.records[slot] = Some(FutexWaiterRecord {
        key,
        tid,
        tcb: tcb_idx as u16,
        va,''')
patch(F, '''            match tcbs.iter_mut().flatten().find(|t| {
                t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
            }) {''', '''            // O(1): the record names its TCB slot; the tid and the token make the match exact.
            match tcbs
                .get_mut(usize::from(rec.tcb))
                .and_then(Option::as_mut)
                .filter(|t| {
                    t.tid.0 == rec.tid && t.status == TaskStatus::Blocked(WaitReason::Futex(token))
                }) {''')
# Dead-code gating: test- and witness-only readers.
patch(F, '''    /// The key a LIVE token's record names.
    pub fn key_of(''', '''    /// The key a LIVE token's record names.
    #[cfg(test)]
    pub fn key_of(''')
patch(F, '''    pub fn va_of(''', '''    #[cfg(any(
        test,
        feature = "aarch64-overtaken-witness",
        feature = "x86-overtaken-witness"
    ))]
    pub fn va_of(''')
patch(F, '''    pub fn occupied(''', '''    #[cfg(test)]
    pub fn occupied(''')
patch(F, '''/// rank 2 — how many LIVE waiters `key` has. Test and witness use.
pub fn live_futex_waiters_locked(''', '''/// rank 2 — how many LIVE waiters `key` has. Test use.
#[cfg(test)]
pub fn live_futex_waiters_locked(''')
patch(F, '''/// rank 2 — how many LIVE waiters named `va` (any key). Test use.
pub fn live_futex_waiters_at_va_locked(''', '''/// rank 2 — how many LIVE waiters named `va` (any key). Test use.
#[cfg(test)]
pub fn live_futex_waiters_at_va_locked(''')
patch(F, '''    addr % core::mem::size_of::<u32>() == 0''', '''    addr.is_multiple_of(core::mem::size_of::<u32>())''')
# The split-only delegate follows the split-only convention.
patch("src/kernel/syscall/sched.rs", '''pub(crate) fn futex_word_range_check(addr: usize)''',
      '''#[cfg_attr(feature = "hosted-dev", allow(dead_code))]
pub(crate) fn futex_word_range_check(addr: usize)''')
# The projector's tuple gets a name.
O = "src/kernel/boot/orchestrator_state.rs"
patch(O, '''    pub(crate) unsafe fn task_futex_split_ptrs_from_raw(
        state: *mut KernelState,
    ) -> (
        *const crate::kernel::lock::SpinLockIrq<()>,
        *mut KernelStorage<[Option<ThreadControlBlock>; MAX_TASKS]>,
        *mut KernelStorage<super::futex::FutexWaiterTable>,
    ) {''', '''    pub(crate) unsafe fn task_futex_split_ptrs_from_raw(
        state: *mut KernelState,
    ) -> TaskFutexSplitPtrs {''')
s = (wt / O).read_text()
i = s.index("impl KernelState {")
(wt / O).write_text(s[:i] + '''/// FUTEX-FOUND1 — the task lock, the TCB array and the futex waiter table, projected together.
pub(crate) type TaskFutexSplitPtrs = (
    *const crate::kernel::lock::SpinLockIrq<()>,
    *mut KernelStorage<[Option<ThreadControlBlock>; MAX_TASKS]>,
    *mut KernelStorage<super::futex::FutexWaiterTable>,
);

''' + s[i:])
print("patch F applied")
